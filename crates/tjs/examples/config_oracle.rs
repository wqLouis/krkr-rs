//! Records what the engine answers for the name `Config`.
//!
//! The recorder asks the global object for `Config` and for the settings a
//! game's configuration script usually defines. Then it defines the name in a
//! script and asks again: a script dictionary answers `Object`, which is what
//! distinguishes a game's configuration from a native class.
//!
//! The record is written to `tests/fixtures/config_members.txt` atomically:
//! the whole table is written beside the fixture and renamed over it, so a
//! failed run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example config_oracle
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
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/config_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The script that gives the probes their helper.
const SETUP: &str = r#"function configProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
"#;

/// What one probe asks for.
#[derive(Clone, Copy)]
enum Mode {
    /// The expression answers a value that `configProbe` wraps.
    Value,
    /// The expression answers a `typeof` text directly.
    Surface,
}

/// The probes whose answer is a `typeof`.
const SURFACE: &[(&str, &str)] = &[
    ("surface_config", "typeof (this[\"Config\"])"),
    (
        "surface_script_config",
        "(function () { var Config = %[]; return typeof (Config); })()",
    ),
    ("surface_screen_width", "typeof (this[\"screenWidth\"])"),
    ("surface_throttle", "typeof (this[\"throttle\"])"),
    (
        "deferred_surface_math_generator",
        "typeof (Math[\"RandomGenerator\"])",
    ),
];

/// The probes whose answer is a value.
const VALUES: &[(&str, &str)] = &[
    (
        "value_script_config_screen_width",
        "(function () { var Config = %[\"screenWidth\" => 1280]; return configProbe(Config[\"screenWidth\"]); })()",
    ),
    (
        "value_global_config_screen_width",
        "(function () { this[\"Config\"] = %[\"screenWidth\" => 1280]; return configProbe(this[\"Config\"][\"screenWidth\"]); })()",
    ),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "config_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in SURFACE {
        record_probe(&engine, &mut table, probe, expression, Mode::Surface);
    }
    for &(probe, expression) in VALUES {
        record_probe(&engine, &mut table, probe, expression, Mode::Value);
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/config_members.txt")
}

/// Evaluates one probe, which must compile.
fn answer(engine: &Tjs2Engine, expression: &str, probe: &str, mode: Mode) -> (String, String) {
    if let Err(message) = compiles(expression) {
        fail(format_args!(
            "probe {probe}: the expression does not compile (!compile): {message}"
        ));
    }
    match engine.eval(expression, "config_members") {
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
    eprintln!("config_oracle: {message}");
    std::process::exit(1)
}
