//! The trace a failure carries through the Rust executor.
//!
//! A trace is built while a failure unwinds: the throw instruction records the
//! frame it was raised in, and every call it crosses records the caller at the
//! call instruction. The checks below drive real scripts through
//! [`Machine::run_context_traced`] and through the public script entry, so the
//! lines they read are the lines a handler or the machine boundary sees.
//!
//! Reference: `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1304-1308`
//! (`ExecuteCode` adds the context it is leaving) and
//! `reference/krkr2/cpp/core/tjs2/tjsError.cpp:108-116` (each line is appended
//! after the previous one, so the trace is innermost first).

use std::rc::Rc;

use tjs::Tjs2Engine;
use tjs::vm::codegen::opcode::vm;
use tjs::vm::compile;
use tjs::vm::runtime::bridge::global_members;
use tjs::vm::runtime::engine;
use tjs::vm::runtime::exec::build::Block;
use tjs::vm::runtime::exec::decode;
use tjs::vm::runtime::exec::throw::{FrameTrace, Unwind, message_of};
use tjs::vm::runtime::exec::{Machine, Raise};
use tjs::vm::runtime::object::{CustomObject, Dispatch};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The label the compiled block carries; the top-level context's trace line
/// carries it too.
const BLOCK: &str = "throw_trace";

/// A script that throws three calls down and lets the failure leave the run.
const THREE_DEEP: &str = r#"
function inner() { throw "boom"; }
function middle() { inner(); }
function outer() { middle(); }
outer();
"#;

/// The script's block, compiled the way the machine entry compiles it.
fn block_of(script: &str) -> Block {
    let image = compile(script, BLOCK).expect("the script compiles");
    Block::build(&image).expect("the block builds")
}

/// Runs the block's top-level context in a machine with a global object.
fn run(block: &Block) -> Result<Variant, Unwind> {
    let global: Rc<dyn Dispatch> = Rc::new(CustomObject::new([TjsString::from("global")]));
    let mut machine = Machine::with_global(global);
    let top = block.top_level().expect("the script has a top level");
    machine.run_context_traced(top.object(), None, &[])
}

/// The text a script returns, which is where a handler puts its trace.
fn answer(block: &Block) -> String {
    match run(block) {
        Ok(Variant::String(text)) => text.to_string(),
        Ok(other) => panic!("the script answered {other:?}, not the trace text"),
        Err(unwind) => panic!("the script failed: {:?}", unwind.trace()),
    }
}

/// The instruction positions in `context` whose opcode is one of `opcodes`.
fn instruction_positions(block: &Block, context: &str, opcodes: &[i32]) -> Vec<usize> {
    let record = block
        .contexts()
        .iter()
        .find(|record| record.object().name().to_string() == context)
        .unwrap_or_else(|| panic!("the block has no context named {context}"));
    let code = &record.object().code().code;
    let mut positions = Vec::new();
    let mut ip = 0;
    while ip < code.len() {
        let opcode = decode::word(code, ip).expect("the code position is a word");
        if opcodes.contains(&opcode) {
            positions.push(ip);
        }
        ip += decode::length(code, ip).expect("the instruction has a length");
    }
    positions
}

/// The one call instruction in `context`.
fn call_position(block: &Block, context: &str) -> usize {
    let positions = instruction_positions(block, context, &[vm::CALL, vm::CALLD, vm::CALLI]);
    assert_eq!(
        positions.len(),
        1,
        "the context {context} has {} call instructions, not one",
        positions.len()
    );
    positions[0]
}

/// The one throw instruction in `context`.
fn throw_position(block: &Block, context: &str) -> usize {
    let positions = instruction_positions(block, context, &[vm::THROW]);
    assert_eq!(
        positions.len(),
        1,
        "the context {context} has {} throw instructions, not one",
        positions.len()
    );
    positions[0]
}

/// The names a trace carries, innermost first.
fn names(trace: &[FrameTrace]) -> Vec<&str> {
    trace.iter().map(|frame| frame.name.as_str()).collect()
}

/// The text lines a trace carries, innermost first.
fn lines(trace: &[FrameTrace]) -> Vec<String> {
    trace
        .iter()
        .map(|frame| {
            if frame.name.is_empty() {
                format!("at code position {}", frame.position)
            } else {
                format!("{} at code position {}", frame.name, frame.position)
            }
        })
        .collect()
}

/// The trace lines a frame list should produce.
fn expected(frames: &[(&str, usize)]) -> Vec<String> {
    frames
        .iter()
        .map(|(name, position)| {
            if name.is_empty() {
                format!("at code position {position}")
            } else {
                format!("{name} at code position {position}")
            }
        })
        .collect()
}

/// The trace text a handler returned.
fn trace_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.lines().collect()
    }
}

/// A throw three calls down passes every call on its way out, so the trace is
/// the throw's frame followed by each caller at its call instruction.
#[test]
fn a_throw_three_calls_deep_records_one_line_per_frame() {
    let block = block_of(THREE_DEEP);
    let unwind = run(&block).expect_err("the throw leaves the top-level run");
    let frames = [
        ("inner", throw_position(&block, "inner")),
        ("middle", call_position(&block, "middle")),
        ("outer", call_position(&block, "outer")),
        (BLOCK, call_position(&block, BLOCK)),
    ];
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1306-1308 — each
    // ExecuteCode adds the context it is leaving, at the instruction it was at.
    // reference/krkr2/cpp/core/tjs2/tjsError.cpp:108-116 — AddTrace appends, so
    // the innermost frame comes first.
    assert_eq!(names(unwind.trace()), ["inner", "middle", "outer", BLOCK]);
    assert_eq!(lines(unwind.trace()), expected(&frames));
}

/// The same chain caught in the middle frame stops there: the outer frames were
/// never crossed and are not in the trace.
#[test]
fn a_failure_caught_in_the_middle_frame_records_only_the_frames_it_crossed() {
    let caught = r#"
function zero_seven() { return 7; }
function zero_zero() { return 0; }
function inner() { return zero_seven() \ zero_zero(); }
function middle() {
    try { inner(); } catch (e) { return e.trace; }
}
function outer() { return middle(); }
return outer();
"#;
    let block = block_of(caught);
    let trace = answer(&block);
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1357-1368 — a caught
    // engine failure becomes an Exception carrying the trace built on the way
    // to this frame's try.
    assert_eq!(
        trace_lines(&trace),
        expected(&[("middle", call_position(&block, "middle"))])
    );
    let caught_count = trace_lines(&trace).len();

    let top = r#"
function zero_seven() { return 7; }
function zero_zero() { return 0; }
function inner() { return zero_seven() \ zero_zero(); }
function middle() { inner(); }
function outer() { middle(); }
try { outer(); } catch (e) { return e.trace; }
"#;
    let block = block_of(top);
    let trace = answer(&block);
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1304-1308 — the two
    // frames the middle catch did not cross are absent from its trace and
    // present in this one.
    let full = [
        ("middle", call_position(&block, "middle")),
        ("outer", call_position(&block, "outer")),
        (BLOCK, call_position(&block, BLOCK)),
    ];
    assert_eq!(trace_lines(&trace), expected(&full));
    assert_eq!(
        full.len() - caught_count,
        2,
        "the middle catch did not cross the outer and top-level frames"
    );
}

/// A rethrow keeps the caught object's own trace, but the rethrow itself starts
/// a new trace from its throw instruction.
#[test]
fn a_rethrow_keeps_the_object_trace_and_resets_the_unwind_trace() {
    let caught = r#"
function zero_seven() { return 7; }
function zero_zero() { return 0; }
function inner() { return zero_seven() \ zero_zero(); }
function middle() {
    try { inner(); } catch (e) { throw e; }
}
function outer() { middle(); }
try { outer(); } catch (e) { return e.trace; }
"#;
    let block = block_of(caught);
    let trace = answer(&block);
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1357-1360 — a script
    // throw hands its caught value to the handler unchanged, so the Exception
    // object keeps the trace it was built with.
    assert_eq!(
        trace_lines(&trace),
        expected(&[("middle", call_position(&block, "middle"))])
    );

    let uncaught = r#"
function zero_seven() { return 7; }
function zero_zero() { return 0; }
function inner() { return zero_seven() \ zero_zero(); }
function middle() {
    try { inner(); } catch (e) { throw e; }
}
function outer() { middle(); }
outer();
"#;
    let block = block_of(uncaught);
    let unwind = run(&block).expect_err("the rethrow leaves the top-level run");
    // reference/krkr2/cpp/core/tjs2/tjsError.cpp:79-81 — the rethrow constructs
    // a new script exception with an empty trace, so its first line is the
    // rethrow's own throw instruction, not the original failure's call.
    let frames = [
        ("middle", throw_position(&block, "middle")),
        ("outer", call_position(&block, "outer")),
        (BLOCK, call_position(&block, BLOCK)),
    ];
    assert_eq!(names(unwind.trace()), ["middle", "outer", BLOCK]);
    assert_eq!(lines(unwind.trace()), expected(&frames));
}

/// A native failure becomes an Exception whose message is the engine's and
/// whose trace names no frame of its own: the raise itself records no line.
#[test]
fn a_native_failure_carries_the_message_and_no_raise_frame() {
    let script = r#"
function zero_seven() { return 7; }
function zero_zero() { return 0; }
try { zero_seven() \ zero_zero(); } catch (e) { return e.message + "|" + e.trace; }
"#;
    let block = block_of(script);
    let text = answer(&block);
    // reference/krkr2/cpp/core/tjs2/tjsError.h:308 — the engine's division
    // message. reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1309-1315,
    // :1361-1368 — the raising context converts the failure to a script error
    // without adding a trace line, so this catch's Exception has an empty trace.
    assert_eq!(
        text,
        format!("{}|", message_of(&Raise::DivideByZero.into()))
    );
}

/// The machine boundary keeps the whole chain, so the diagnostic and a handler
/// read the same frames.
#[test]
fn an_uncaught_failure_keeps_the_whole_chain_at_the_machine_boundary() {
    let block = block_of(THREE_DEEP);
    let expected_frames = [
        ("inner", throw_position(&block, "inner")),
        ("middle", call_position(&block, "middle")),
        ("outer", call_position(&block, "outer")),
        (BLOCK, call_position(&block, BLOCK)),
    ];
    let engine = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    let failure = engine::run_with_members(engine, THREE_DEEP, BLOCK, global_members())
        .expect_err("the throw is uncaught");
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1304-1308 — the
    // uncaught path is the same accumulated trace, not a fresh one-line report.
    assert_eq!(names(&failure.trace), ["inner", "middle", "outer", BLOCK]);
    assert_eq!(lines(&failure.trace), expected(&expected_frames));
}
