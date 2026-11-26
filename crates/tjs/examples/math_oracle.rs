//! Records what the engine answers for the `Math` class's members.
//!
//! The recorder probes the class's surface one name at a time, then calls each
//! function with the arguments its rules turn on: an integer, a real, a text,
//! the empty value, an object, an octet, a missing parameter and an extra one.
//! Every call goes through a helper function, so a constant folder cannot
//! answer it, and one answer is recorded per probe: the kind the language's
//! `typeof` names and the value as text. A probe the language raises records
//! the raise as `!raise`: the raise is the rule's answer, not a gap in the
//! record.
//!
//! The `random` member and the members the class object itself owns are
//! recorded but not replayed by the check: their state belongs to the class,
//! not to this table of functions.
//!
//! The record is written to `tests/fixtures/math_members.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example math_oracle
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
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/math_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The script that gives the probes their helpers.
///
/// The builders are named functions because a script cannot declare one inside
/// an anonymous body. `mathProbe` answers the kind and the text in one call, so
/// a probe is evaluated once; `mathRandomRange` asks the generator's rule
/// without pinning a number the clock seeded.
const SETUP: &str = r#"function mathProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function mathRandomRange() {
    var value = Math.random();
    return value >= 0.0 && value < 1.0;
}
"#;

/// What one probe asks for.
#[derive(Clone, Copy)]
enum Mode {
    /// The expression answers the `typeof` of a member.
    Surface,
    /// The expression answers a value that `mathProbe` wraps.
    Value,
}

/// The class's own members, which this module's table must publish.
const SURFACE: &[(&str, &str)] = &[
    ("surface_abs", "abs"),
    ("surface_acos", "acos"),
    ("surface_asin", "asin"),
    ("surface_atan", "atan"),
    ("surface_atan2", "atan2"),
    ("surface_ceil", "ceil"),
    ("surface_cos", "cos"),
    ("surface_exp", "exp"),
    ("surface_floor", "floor"),
    ("surface_log", "log"),
    ("surface_max", "max"),
    ("surface_min", "min"),
    ("surface_pow", "pow"),
    ("surface_round", "round"),
    ("surface_sin", "sin"),
    ("surface_sqrt", "sqrt"),
    ("surface_tan", "tan"),
    ("surface_E", "E"),
    ("surface_LOG2E", "LOG2E"),
    ("surface_LOG10E", "LOG10E"),
    ("surface_LN10", "LN10"),
    ("surface_LN2", "LN2"),
    ("surface_PI", "PI"),
    ("surface_SQRT1_2", "SQRT1_2"),
    ("surface_SQRT2", "SQRT2"),
];

/// The members the class object owns, whose rules are another part's.
const DEFERRED_SURFACE: &[(&str, &str)] = &[
    ("deferred_surface_finalize", "finalize"),
    ("deferred_surface_random", "random"),
    ("deferred_surface_RandomGenerator", "RandomGenerator"),
];

/// Names a script might expect but the class does not publish.
const ABSENT: &[(&str, &str)] = &[
    ("absent_cbrt", "cbrt"),
    ("absent_hypot", "hypot"),
    ("absent_int", "int"),
    ("absent_log10", "log10"),
    ("absent_log2", "log2"),
    ("absent_octetVectorSum", "octetVectorSum"),
    ("absent_percent", "%"),
    ("absent_rand", "rand"),
    ("absent_sign", "sign"),
    ("absent_transcendental", "transcendental"),
    ("absent_trunc", "trunc"),
];

/// The calls whose answer the check replays.
const VALUES: &[(&str, &str)] = &[
    ("value_constant_e", "Math.E"),
    ("value_constant_log2e", "Math.LOG2E"),
    ("value_constant_log10e", "Math.LOG10E"),
    ("value_constant_ln10", "Math.LN10"),
    ("value_constant_ln2", "Math.LN2"),
    ("value_constant_pi", "Math.PI"),
    ("value_constant_sqrt1_2", "Math.SQRT1_2"),
    ("value_constant_sqrt2", "Math.SQRT2"),
    ("value_abs_integer", "Math.abs(-5)"),
    ("value_abs_real", "Math.abs(-1.5)"),
    ("value_abs_negative_zero", "Math.abs(-0.0)"),
    ("value_abs_text", r#"Math.abs("-5")"#),
    ("value_abs_word", r#"Math.abs("abc")"#),
    ("value_abs_empty_text", r#"Math.abs("")"#),
    ("value_abs_void", "Math.abs(void)"),
    ("value_abs_extra", "Math.abs(-2, 3)"),
    ("value_abs_missing", "Math.abs()"),
    ("value_abs_object", "Math.abs(Math)"),
    ("value_abs_octet", "Math.abs(<%00%>)"),
    ("value_floor_positive", "Math.floor(3.7)"),
    ("value_floor_negative", "Math.floor(-3.2)"),
    ("value_floor_integer", "Math.floor(3)"),
    ("value_floor_text", r#"Math.floor("3.7")"#),
    ("value_floor_missing", "Math.floor()"),
    ("value_ceil_positive", "Math.ceil(3.2)"),
    ("value_ceil_negative", "Math.ceil(-3.7)"),
    ("value_ceil_integer", "Math.ceil(3)"),
    ("value_ceil_missing", "Math.ceil()"),
    ("value_round_half", "Math.round(2.5)"),
    ("value_round_half_negative", "Math.round(-2.5)"),
    ("value_round_below_half", "Math.round(2.4999)"),
    ("value_round_negative_zero", "Math.round(-0.5)"),
    ("value_round_negative_small", "Math.round(-0.6)"),
    ("value_round_text", r#"Math.round("2.5")"#),
    ("value_round_missing", "Math.round()"),
    ("value_sin_zero", "Math.sin(0)"),
    ("value_sin_half_pi", "Math.sin(Math.PI / 2)"),
    ("value_sin_text", r#"Math.sin("0")"#),
    ("value_sin_missing", "Math.sin()"),
    ("value_cos_zero", "Math.cos(0)"),
    ("value_cos_pi", "Math.cos(Math.PI)"),
    ("value_cos_missing", "Math.cos()"),
    ("value_tan_zero", "Math.tan(0)"),
    ("value_tan_quarter_pi", "Math.tan(Math.PI / 4)"),
    ("value_tan_missing", "Math.tan()"),
    ("value_asin_one", "Math.asin(1)"),
    ("value_asin_domain", "Math.asin(2)"),
    ("value_asin_missing", "Math.asin()"),
    ("value_acos_one", "Math.acos(1)"),
    ("value_acos_domain", "Math.acos(2)"),
    ("value_acos_missing", "Math.acos()"),
    ("value_atan_one", "Math.atan(1)"),
    ("value_atan_text", r#"Math.atan("1")"#),
    ("value_atan_missing", "Math.atan()"),
    ("value_atan2_quarter", "Math.atan2(1, 1)"),
    ("value_atan2_text", r#"Math.atan2("2", "1")"#),
    ("value_atan2_zero_zero", "Math.atan2(0, 0)"),
    ("value_atan2_negative_zero", "Math.atan2(-0.0, 1)"),
    ("value_atan2_one_argument", "Math.atan2(1)"),
    ("value_atan2_missing", "Math.atan2()"),
    ("value_exp_zero", "Math.exp(0)"),
    ("value_exp_one", "Math.exp(1)"),
    ("value_exp_text", r#"Math.exp("0")"#),
    ("value_exp_missing", "Math.exp()"),
    ("value_log_one", "Math.log(1)"),
    ("value_log_e", "Math.log(Math.E)"),
    ("value_log_zero", "Math.log(0)"),
    ("value_log_negative", "Math.log(-1)"),
    ("value_log_missing", "Math.log()"),
    ("value_pow_integer", "Math.pow(2, 10)"),
    ("value_pow_negative", "Math.pow(2, -1)"),
    ("value_pow_fractional", "Math.pow(9, 0.5)"),
    ("value_pow_domain", "Math.pow(-2, 0.5)"),
    ("value_pow_zero_zero", "Math.pow(0, 0)"),
    ("value_pow_text", r#"Math.pow("2", "3")"#),
    ("value_pow_one_argument", "Math.pow(2)"),
    ("value_pow_missing", "Math.pow()"),
    ("value_sqrt_integer", "Math.sqrt(9)"),
    ("value_sqrt_two", "Math.sqrt(2)"),
    ("value_sqrt_negative", "Math.sqrt(-1)"),
    ("value_sqrt_text", r#"Math.sqrt("16")"#),
    ("value_sqrt_missing", "Math.sqrt()"),
    ("value_max_none", "Math.max()"),
    ("value_max_one", "Math.max(3)"),
    ("value_max_mixed", "Math.max(1, 2.5, 2)"),
    ("value_max_negative", "Math.max(-3, -2)"),
    ("value_max_text", r#"Math.max("10", 9)"#),
    ("value_max_positive_zero", "Math.max(-0.0, 0.0)"),
    ("value_max_negative_zero", "Math.max(-0.0, -0.0)"),
    ("value_max_nan", "Math.max(1, 0.0 / 0.0, 2)"),
    ("value_max_object", "Math.max(1, Math)"),
    ("value_min_none", "Math.min()"),
    ("value_min_one", "Math.min(3)"),
    ("value_min_mixed", "Math.min(1, 2.5, 2)"),
    ("value_min_positive_zero", "Math.min(0.0, -0.0)"),
    ("value_min_negative_zero", "Math.min(-0.0, -0.0)"),
    ("value_min_nan", "Math.min(1, 0.0 / 0.0)"),
    ("value_min_text", r#"Math.min("10", 9)"#),
    ("value_min_octet", "Math.min(<%00%>)"),
];

/// The probes whose rules belong to the class object, recorded but not
/// replayed: the member's name, the expression, and the mode.
const DEFERRED_VALUES: &[(&str, &str, Mode)] = &[
    (
        "deferred_value_random_kind",
        "typeof (Math.random())",
        Mode::Surface,
    ),
    (
        "deferred_value_random_range",
        "mathRandomRange()",
        Mode::Value,
    ),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "math_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, member) in SURFACE {
        let expression = format!("typeof (Math[{member:?}])");
        record_probe(&engine, &mut table, probe, &expression, Mode::Surface);
    }
    for &(probe, member) in DEFERRED_SURFACE {
        let expression = format!("typeof (Math[{member:?}])");
        record_probe(&engine, &mut table, probe, &expression, Mode::Surface);
    }
    for &(probe, member) in ABSENT {
        let expression = format!("typeof (Math[{member:?}])");
        record_probe(&engine, &mut table, probe, &expression, Mode::Surface);
    }
    for &(probe, expression) in VALUES {
        let call = format!("mathProbe({expression})");
        record_probe(&engine, &mut table, probe, &call, Mode::Value);
    }
    for &(probe, expression, mode) in DEFERRED_VALUES {
        let call = match mode {
            Mode::Surface => expression.to_owned(),
            Mode::Value => format!("mathProbe({expression})"),
        };
        record_probe(&engine, &mut table, probe, &call, mode);
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/math_members.txt")
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
    match engine.eval(expression, "math_members") {
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
    eprintln!("math_oracle: {message}");
    std::process::exit(1)
}
