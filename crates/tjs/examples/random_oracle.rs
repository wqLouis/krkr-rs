//! Records what the engine answers for the `Math.RandomGenerator` class and
//! for `Math.random`.
//!
//! The recorder probes the class's surface one name at a time, then calls the
//! members that a seeded generator makes reproducible: an integer seed, a real,
//! a text, the empty value, an object, an octet, a missing parameter and an
//! extra one. A draw is recorded as the sequence of answers, so the state and
//! the tempering are both evidence. The serialised state is recorded whole,
//! with the two counters that place the next answer.
//!
//! A probe the language raises records the raise as `!raise`: the raise is the
//! rule's answer, not a gap in the record. A member whose seed is the clock —
//! `new Math.RandomGenerator()` and `randomize()` with no argument — is
//! recorded by its range only, because no two runs draw the same sequence.
//!
//! The record is written to `tests/fixtures/random_members.txt` atomically:
//! the whole table is written beside the fixture and renamed over it, so a
//! failed run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example random_oracle
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
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/random_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The script that gives the probes their helpers.
///
/// The builders are named functions because a script cannot declare one inside
/// an anonymous body. `randomProbe` answers the kind and the text in one call,
/// so a probe is evaluated once.
const SETUP: &str = r#"function randomProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function randomDraw(seed, which, count) {
    var r = new Math.RandomGenerator(seed);
    var out = "";
    var i;
    for (i = 0; i < count; ++i) {
        if (i != 0) out += ",";
        if (which == 1) out += r.random();
        else if (which == 32) out += r.random32();
        else if (which == 63) out += r.random63();
        else out += r.random64();
    }
    return out;
}
function randomOne(seed) {
    var r = new Math.RandomGenerator(seed);
    return r.random32();
}
function randomOneExtra(seed) {
    var r = new Math.RandomGenerator(seed, 2, 3);
    return r.random32();
}
function randomReseed(seed, next) {
    var r = new Math.RandomGenerator(seed);
    r.random32();
    r.randomize(next);
    return r.random32();
}
function randomReseedExtra(seed, next) {
    var r = new Math.RandomGenerator(seed);
    r.random32();
    r.randomize(next, 3, 4);
    return r.random32();
}
function randomSerialized(seed, count) {
    var r = new Math.RandomGenerator(seed);
    var i;
    for (i = 0; i < count; ++i) r.random32();
    var d = r.serialize();
    return d.state + "|" + d.left + "|" + d.next;
}
function randomSerializedShape(seed) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    return (typeof (d)) + ":" + (typeof (d.state)) + ":" + (d.state.length) +
        ":" + (typeof (d.left)) + ":" + d.left +
        ":" + (typeof (d.next)) + ":" + d.next;
}
function randomRestored(seed, count) {
    var r = new Math.RandomGenerator(seed);
    var i;
    for (i = 0; i < count; ++i) r.random32();
    var d = r.serialize();
    r.randomize(d);
    return r.random32() + "," + r.random32();
}
function randomRestoredFromNew(seed, count) {
    var r = new Math.RandomGenerator(seed);
    var i;
    for (i = 0; i < count; ++i) r.random32();
    var d = r.serialize();
    var s = new Math.RandomGenerator(d);
    return s.random32() + "," + s.random32();
}
function randomRestoredRandom(seed, count) {
    var r = new Math.RandomGenerator(seed);
    var i;
    for (i = 0; i < count; ++i) r.random();
    var d = r.serialize();
    r.randomize(d);
    return r.random();
}
function randomStateUppercase(seed) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    d.state = d.state.toUpperCase();
    r.randomize(d);
    return r.random32();
}
function randomMutated(seed, left, next, count) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    d.left = left;
    d.next = next;
    r.randomize(d);
    if (count == 2) return r.random32() + "," + r.random32();
    return r.random32();
}
function randomShortState(seed) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    d.state = "00";
    r.randomize(d);
    return r.random32();
}
function randomNonHexState(seed) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    d.state = "z" + d.state.substring(1);
    r.randomize(d);
    return r.random32();
}
function randomIntState(seed) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    d.state = 123;
    r.randomize(d);
    return r.random32();
}
function randomOctetState(seed) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    d.state = <%00%>;
    r.randomize(d);
    return r.random32();
}
function randomVoidState(seed) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    d.state = void;
    r.randomize(d);
    return r.random32();
}
function randomMissing(seed, which) {
    var r = new Math.RandomGenerator(seed);
    var d = r.serialize();
    if (which == 0) delete d.state;
    else if (which == 1) delete d.left;
    else delete d.next;
    r.randomize(d);
    return r.random32();
}
function randomCtorOctet() {
    var r = new Math.RandomGenerator(<%00%>);
    return r.random32();
}
function randomCtorNull() {
    var r = new Math.RandomGenerator(null);
    return r.random32();
}
function randomCtorDictionary() {
    var r = new Math.RandomGenerator(%[]);
    return r.random32();
}
function randomizeOctet(seed) {
    var r = new Math.RandomGenerator(seed);
    r.randomize(<%00%>);
    return r.random32();
}
function randomizeNull(seed) {
    var r = new Math.RandomGenerator(seed);
    r.randomize(null);
    return r.random32();
}
function randomizeDictionary(seed) {
    var r = new Math.RandomGenerator(seed);
    r.randomize(%[]);
    return r.random32();
}
function randomizeReturns(seed) {
    var r = new Math.RandomGenerator(seed);
    return typeof (r.randomize(2));
}
function finalizeReturns(seed) {
    var r = new Math.RandomGenerator(seed);
    return typeof (r.finalize());
}
function randomExtra(seed, which) {
    var r = new Math.RandomGenerator(seed);
    if (which == 1) return r.random(9, 8);
    else if (which == 32) return r.random32(9, 8);
    else if (which == 63) return r.random63(9, 8);
    else return r.random64(9, 8);
}
function randomNoArgInRange() {
    var r = new Math.RandomGenerator();
    var value = r.random();
    return value >= 0 && value < 1;
}
function randomReseedNoArgInRange() {
    var r = new Math.RandomGenerator(1);
    r.randomize();
    var value = r.random();
    return value >= 0 && value < 1;
}
function mathRandomRange() {
    var value = Math.random();
    return value >= 0 && value < 1;
}
function mathRandomMany() {
    var i;
    var value;
    for (i = 0; i < 100; ++i) {
        value = Math.random();
        if (!(value >= 0 && value < 1)) return 0;
    }
    return 1;
}
"#;

/// What one probe asks for.
#[derive(Clone, Copy)]
enum Mode {
    /// The expression answers a value that `randomProbe` wraps.
    Value,
    /// The expression answers a text directly.
    Surface,
}

/// The class's own members, which this module's table must publish.
const SURFACE: &[(&str, &str)] = &[
    ("surface_finalize", "finalize"),
    ("surface_randomize", "randomize"),
    ("surface_random", "random"),
    ("surface_random32", "random32"),
    ("surface_random63", "random63"),
    ("surface_random64", "random64"),
    ("surface_serialize", "serialize"),
];

/// Names a script might expect but the class does not publish.
const ABSENT: &[(&str, &str)] = &[
    ("absent_class", "class"),
    ("absent_constructor", "constructor"),
    ("absent_init", "init"),
    ("absent_left", "left"),
    ("absent_next", "next"),
    ("absent_rand", "rand"),
    ("absent_random53", "random53"),
    ("absent_random_double", "randomDouble"),
    ("absent_randomize32", "randomize32"),
    ("absent_seed", "seed"),
    ("absent_set_state", "setState"),
    ("absent_state", "state"),
];

/// The members the class object owns, recorded but not replayed.
const DEFERRED_SURFACE: &[(&str, &str)] = &[
    ("deferred_surface_math_random", "typeof (Math[\"random\"])"),
    (
        "deferred_surface_math_random_generator",
        "typeof (Math[\"RandomGenerator\"])",
    ),
];

/// The `typeof` answers that are not member names.
const KINDS: &[(&str, &str)] = &[
    ("kind_constructor", "typeof (new Math.RandomGenerator(1))"),
    ("kind_math_random", "typeof (Math.random())"),
    ("kind_math_random_extra", "typeof (Math.random(1, 2))"),
];

/// The calls whose answer the check replays.
const VALUES: &[(&str, &str)] = &[
    ("value_seed_1_random_4", "randomDraw(1, 1, 4)"),
    ("value_seed_1_random32_6", "randomDraw(1, 32, 6)"),
    ("value_seed_1_random63_3", "randomDraw(1, 63, 3)"),
    ("value_seed_1_random64_3", "randomDraw(1, 64, 3)"),
    ("value_seed_0_random32", "randomOne(0)"),
    ("value_seed_neg1_random32", "randomOne(-1)"),
    ("value_seed_2p32_random32", "randomOne(4294967296)"),
    ("value_seed_real_random32", "randomOne(1.9)"),
    ("value_seed_text_word_random32", "randomOne(\"abc\")"),
    ("value_seed_text_one_random32", "randomOne(\"1\")"),
    ("value_seed_empty_random32", "randomOne(\"\")"),
    ("value_seed_void_random32", "randomOne(void)"),
    ("value_seed_extra_random32", "randomOneExtra(1)"),
    ("value_reseed_seed_random32", "randomReseed(1, 2)"),
    ("value_reseed_extra_random32", "randomReseedExtra(1, 2)"),
    ("value_reseed_text_random32", "randomReseed(1, \"abc\")"),
    ("value_reseed_void_random32", "randomReseed(1, void)"),
    ("value_reseed_negative_random32", "randomReseed(1, -1)"),
    ("value_serialize_shape", "randomSerializedShape(1)"),
    ("value_serialize_fresh", "randomSerialized(1, 0)"),
    ("value_serialize_after_2", "randomSerialized(1, 2)"),
    ("value_serialize_after_random", "randomSerialized(9, 2)"),
    ("value_restore_continues", "randomRestored(7, 2)"),
    ("value_restore_from_new", "randomRestoredFromNew(7, 2)"),
    ("value_restore_after_random", "randomRestoredRandom(9, 1)"),
    ("value_restore_uppercase", "randomStateUppercase(1)"),
    ("value_state_left_text", "randomMutated(1, \"1\", 0, 1)"),
    ("value_state_left_word", "randomMutated(1, \"abc\", 0, 1)"),
    ("value_state_left_real", "randomMutated(1, 1.9, 0, 1)"),
    ("value_state_next_text", "randomMutated(1, 1, \"0\", 1)"),
    ("value_state_left_624", "randomMutated(1, 624, 0, 1)"),
    ("value_state_left_1", "randomMutated(1, 1, 0, 1)"),
    ("value_state_left_0", "randomMutated(1, 0, 0, 1)"),
    ("value_state_left_neg1", "randomMutated(1, -1, 0, 1)"),
    ("value_state_left_625", "randomMutated(1, 625, 0, 1)"),
    ("value_state_left_2_next_3", "randomMutated(1, 2, 3, 2)"),
    ("value_ctor_octet", "randomCtorOctet()"),
    ("value_ctor_null", "randomCtorNull()"),
    ("value_ctor_dictionary", "randomCtorDictionary()"),
    ("value_randomize_octet", "randomizeOctet(1)"),
    ("value_randomize_null", "randomizeNull(1)"),
    ("value_randomize_dictionary", "randomizeDictionary(1)"),
    ("value_randomize_returns", "randomizeReturns(1)"),
    ("value_finalize_returns", "finalizeReturns(1)"),
    ("value_random_extra", "randomExtra(1, 1)"),
    ("value_random32_extra", "randomExtra(1, 32)"),
    ("value_random63_extra", "randomExtra(1, 63)"),
    ("value_random64_extra", "randomExtra(1, 64)"),
    ("value_state_short", "randomShortState(1)"),
    ("value_state_nonhex", "randomNonHexState(1)"),
    ("value_state_integer", "randomIntState(1)"),
    ("value_state_octet", "randomOctetState(1)"),
    ("value_state_void", "randomVoidState(1)"),
    ("value_state_missing_state", "randomMissing(1, 0)"),
    ("value_state_missing_left", "randomMissing(1, 1)"),
    ("value_state_missing_next", "randomMissing(1, 2)"),
];

/// The probes the class object owns, recorded but not replayed.
const DEFERRED_VALUES: &[(&str, &str)] = &[
    ("deferred_value_ctor_default_range", "randomNoArgInRange()"),
    (
        "deferred_value_reseed_default_range",
        "randomReseedNoArgInRange()",
    ),
    ("deferred_value_math_random_range", "mathRandomRange()"),
    ("deferred_value_math_random_many", "mathRandomMany()"),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "random_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, member) in SURFACE {
        let expression = format!("typeof (Math.RandomGenerator[{member:?}])");
        record_probe(&engine, &mut table, probe, &expression, Mode::Surface);
    }
    for &(probe, member) in ABSENT {
        let expression = format!("typeof (Math.RandomGenerator[{member:?}])");
        record_probe(&engine, &mut table, probe, &expression, Mode::Surface);
    }
    for &(probe, expression) in DEFERRED_SURFACE {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    for &(probe, expression) in KINDS {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    for &(probe, expression) in VALUES {
        let call = format!("randomProbe({expression})");
        record_probe(&engine, &mut table, probe, &call, Mode::Value);
    }
    for &(probe, expression) in DEFERRED_VALUES {
        let call = format!("randomProbe({expression})");
        record_probe(&engine, &mut table, probe, &call, Mode::Value);
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/random_members.txt")
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
    match engine.eval(expression, "random_members") {
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
    eprintln!("random_oracle: {message}");
    std::process::exit(1)
}
