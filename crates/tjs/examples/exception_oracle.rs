//! Records what the engine answers for the `Exception` class.
//!
//! The recorder asks the global object for the class and for the members a
//! script might expect, then constructs an exception with no argument, a
//! message, a trace, an integer, a real and the empty value, and reads every
//! member back. It then throws an exception and three non-exception values and
//! records what a handler receives: the value's kind, whether it belongs to
//! the class, and — for an exception — its members. One failure of the engine
//! is recorded through the same handler so that the object a raise becomes is
//! evidence too; the trace is recorded by its presence, not its text, because
//! its positions belong to the block the probe ran in.
//!
//! The record is written to `tests/fixtures/exception_members.txt` atomically:
//! the whole table is written beside the fixture and renamed over it, so a
//! failed run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example exception_oracle
//! ```

use std::ffi::CStr;
use std::fs;
use std::path::PathBuf;
use std::ptr;

use tjs::vm::{
    TJS2_SCRIPT_OK, tjs2_script_compile, tjs2_script_error_message, tjs2_script_free,
    tjs2_script_status,
};
use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/exception_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The script that gives the probes their helpers.
///
/// The builders are named functions because a script cannot declare one inside
/// an anonymous body. `exceptionProbe` answers the kind and the text in one
/// call, so a probe is evaluated once.
const SETUP: &str = r#"function exceptionProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function exceptionCaught(which) {
    try {
        if (which == 0) throw new Exception("thrown");
        else if (which == 1) throw "text";
        else if (which == 2) throw 5;
        else if (which == 3) throw %["a" => 1];
        else if (which == 4) throw new Exception("m", "t");
        else throw new Exception();
    } catch (e) {
        return (typeof (e)) + ":" + ((e instanceof Exception) ? 1 : 0);
    }
    return "nocatch";
}
function exceptionCaughtException(which) {
    try {
        if (which == 0) throw new Exception("thrown");
        else if (which == 1) throw new Exception("m", "t");
        else throw new Exception();
    } catch (e) {
        return (typeof (e.finalize)) + ":" +
            (typeof (e.message)) + ":" + ("" + e.message) + ":" +
            (typeof (e.trace)) + ":" + ("" + e.trace);
    }
    return "nocatch";
}
function exceptionRaised(which) {
    try {
        if (which == 0) {
            var zero = 0;
            var quotient = 7 \ zero;
        } else {
            var missing;
            missing.member;
        }
    } catch (e) {
        return (typeof (e)) + ":" + ((e instanceof Exception) ? 1 : 0) + ":" +
            (typeof (e.finalize)) + ":" +
            (typeof (e.message)) + ":" + ("" + e.message) + ":" +
            (typeof (e.trace)) + ":" + (((e.trace.length) > 0) ? 1 : 0);
    }
    return "nocatch";
}
"#;

/// What one probe asks for.
#[derive(Clone, Copy)]
enum Mode {
    /// The expression answers a text directly.
    Surface,
    /// The expression answers a value that `exceptionProbe` wraps.
    Value,
}

/// The class object's own members, as the engine publishes them.
const SURFACE: &[(&str, &str)] = &[
    ("surface_class", "typeof (Exception)"),
    ("surface_class_constructor", "typeof (Exception.Exception)"),
    ("surface_class_finalize", "typeof (Exception.finalize)"),
    ("surface_class_message", "typeof (Exception.message)"),
    ("surface_class_trace", "typeof (Exception.trace)"),
];

/// Names a script might expect but the class does not publish.
const ABSENT: &[(&str, &str)] = &[
    ("absent_class", "typeof (Exception.class)"),
    ("absent_constructor", "typeof (Exception.constructor)"),
    ("absent_init", "typeof (Exception.init)"),
    ("absent_to_string", "typeof (Exception.toString)"),
];

/// The `typeof` answers of a construction and its members.
const KINDS: &[(&str, &str)] = &[
    ("kind_instance", "typeof (new Exception())"),
    ("kind_message", "typeof ((new Exception()).message)"),
    ("kind_trace", "typeof ((new Exception()).trace)"),
    ("kind_finalize", "typeof ((new Exception()).finalize)"),
    (
        "kind_finalize_call",
        "typeof ((new Exception()).finalize())",
    ),
    (
        "kind_void_message",
        "typeof ((new Exception(void)).message)",
    ),
];

/// The member reads whose answer the check replays.
const VALUES: &[(&str, &str)] = &[
    ("value_default_message", "(new Exception()).message"),
    ("value_default_trace", "(new Exception()).trace"),
    ("value_message_text", "(new Exception(\"m\")).message"),
    ("value_message_integer", "(new Exception(1)).message"),
    ("value_message_real", "(new Exception(1.5)).message"),
    ("value_message_empty", "(new Exception(\"\")).message"),
    ("value_message_void", "(new Exception(void)).message"),
    ("value_trace_text", "(new Exception(\"m\", \"t\")).trace"),
    ("value_trace_integer", "(new Exception(\"m\", 4)).trace"),
    ("value_trace_void", "(new Exception(\"m\", void)).trace"),
    (
        "value_message_extra",
        "(new Exception(\"m\", \"t\", 9)).message",
    ),
    (
        "value_instance_of",
        "(new Exception()) instanceof Exception",
    ),
    (
        "value_instance_of_object",
        "(new Exception()) instanceof Object",
    ),
];

/// The caught values whose answer the check replays.
const CAUGHT: &[(&str, &str)] = &[
    ("caught_exception", "exceptionCaught(0)"),
    ("caught_text", "exceptionCaught(1)"),
    ("caught_integer", "exceptionCaught(2)"),
    ("caught_dictionary", "exceptionCaught(3)"),
    ("caught_exception_two", "exceptionCaught(4)"),
    ("caught_exception_default", "exceptionCaught(5)"),
    ("caught_members", "exceptionCaughtException(0)"),
    ("caught_members_two", "exceptionCaughtException(1)"),
    ("caught_members_default", "exceptionCaughtException(2)"),
];

/// The probes the class object owns, recorded but not replayed.
const DEFERRED: &[(&str, &str)] = &[("deferred_value_raised_divide", "exceptionRaised(0)")];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "exception_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in SURFACE {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    for &(probe, expression) in ABSENT {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    for &(probe, expression) in KINDS {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    for &(probe, expression) in VALUES {
        let call = format!("exceptionProbe({expression})");
        record_probe(&engine, &mut table, probe, &call, Mode::Value);
    }
    for &(probe, expression) in CAUGHT {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    for &(probe, expression) in DEFERRED {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    let path = fixture_path();
    let temporary = path.with_extension("txt.tmp");
    if let Err(error) = fs::write(&temporary, table) {
        fail(format_args!(
            "the temporary record {} is not writable: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        fail(format_args!(
            "the record could not be renamed to {}: {error}",
            path.display()
        ));
    }
    println!("wrote {}", path.display());
}

/// Evaluates one probe and appends its record line.
fn record_probe(
    engine: &Tjs2Engine,
    table: &mut String,
    probe: &str,
    expression: &str,
    mode: Mode,
) {
    let (kind, text) = answer(engine, expression, probe, mode);
    table.push_str(&format!("{probe}\t{kind}\t{text}\n"));
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exception_members.txt")
}

/// Evaluates one probe, which must compile.
///
/// The expression is compiled first, so a probe the front end refuses is told
/// apart from a raise: it never reaches a rule, and the recorder stops rather
/// than record it as the rule's answer.
fn answer(engine: &Tjs2Engine, expression: &str, probe: &str, mode: Mode) -> (String, String) {
    if let Err(message) = compiles(expression) {
        fail(format_args!(
            "probe {probe}: the expression does not compile (!compile): {message}"
        ));
    }
    match engine.eval(expression, "exception_members") {
        Ok(TjsValue::String(text)) => match mode {
            Mode::Surface => (String::from("String"), escaped(&text)),
            Mode::Value => match text.split_once(':') {
                Some((kind, value)) => (kind.to_owned(), escaped(value)),
                None => fail(format_args!(
                    "probe {probe}: the answer has no kind and text: {text:?}"
                )),
            },
        },
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(_raised) => (String::from(RAISED), String::from(RAISED)),
    }
}

/// Whether the front end compiles the expression, and why it does not.
fn compiles(expression: &str) -> Result<(), String> {
    let units: Vec<u16> = expression.encode_utf16().collect();
    let length = u32::try_from(units.len()).expect("a probe expression fits a 32-bit length");
    // SAFETY: the units are readable for `length` code units, the null values
    // pointer means "no conditional-compilation values", and the handle is
    // freed before this returns.
    unsafe {
        let script = tjs2_script_compile(units.as_ptr(), length, 1, 1, ptr::null());
        if script.is_null() {
            return Err(String::from("the front end answered no handle"));
        }
        let status = tjs2_script_status(script);
        if status == TJS2_SCRIPT_OK {
            tjs2_script_free(script);
            return Ok(());
        }
        let message = tjs2_script_error_message(script);
        let message = if message.is_null() {
            String::from("the front end gave no message")
        } else {
            // SAFETY: the handle owns a NUL-terminated message, live until the
            // handle is freed below.
            CStr::from_ptr(message).to_string_lossy().into_owned()
        };
        tjs2_script_free(script);
        Err(message)
    }
}

/// The text escaped so that one probe stays on one line.
///
/// A tab, a newline and a backslash are written as their two-character
/// escapes, and a code unit below `0x20` is written as `\xNN`.
fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\\' => escaped.push_str("\\\\"),
            character if character < ' ' => {
                escaped.push_str(&format!("\\x{:02x}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("exception_oracle: {message}");
    std::process::exit(1)
}
