//! Records the engine's message for each refusal a script can catch.
//!
//! Each probe is a script that forces one refusal inside a `try` and answers
//! `e.message` from the catch. The probes cover the conversion message's value
//! and target, the 512-unit trim with a one-byte and a 174-byte octet, the
//! read-only property's access denial, the two range errors, and the string
//! member's own failures: a parameter count, a bad format string and a
//! parameter of the wrong kind. The value is never a live object's form,
//! because an address is not reproducible; the null object's fixed
//! `(object 0x0:0x0)` is used instead.
//!
//! The record is written to `tests/fixtures/raise_messages.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example raise_oracle
//! ```

use std::fs;
use std::path::PathBuf;

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/raise_oracle.rs`; regenerate with the command in the example's docs";

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for (probe, script) in probes() {
        match engine.exec_script(&script, "raise_messages") {
            Ok(TjsValue::String(text)) => {
                table.push_str(&format!("{probe}\t{}\n", escaped(&text)));
            }
            Ok(other) => fail(format_args!(
                "probe {probe}: the answer is not a text: {other:?}"
            )),
            Err(error) => fail(format_args!("probe {probe}: the script failed: {error}")),
        }
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

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/raise_messages.txt")
}

/// The probe table, in the order the check replays it.
fn probes() -> Vec<(&'static str, String)> {
    let small = octet_literal(1);
    let long = octet_literal(174);
    let string_octet = |body: &str| caught(&format!("var value = {small}; {body}"));
    let long_octet = |body: &str| caught(&format!("var value = {long}; {body}"));
    vec![
        (
            "convert_int_null",
            caught("var value = null; var answer = (int)value;"),
        ),
        (
            "convert_real_null",
            caught("var value = null; var answer = (real)value;"),
        ),
        (
            "convert_string_octet",
            string_octet("var answer = (string)value;"),
        ),
        (
            "convert_int_octet",
            string_octet("var answer = (int)value;"),
        ),
        (
            "convert_real_octet",
            string_octet("var answer = (real)value;"),
        ),
        (
            "convert_number_null",
            caught("var value = null; var answer = value * 1;"),
        ),
        (
            "convert_object_integer",
            caught("var value = 1; var answer = value();"),
        ),
        ("convert_increment_octet", string_octet("value++;")),
        ("convert_increment_octet_long", long_octet("value++;")),
        (
            "convert_string_octet_long",
            long_octet("var answer = (string)value;"),
        ),
        (
            "access_denied_text_length",
            caught("var value = \"abc\"; value.length = 1;"),
        ),
        (
            "range_error_text_index",
            caught("var value = \"abc\"; var answer = value[4];"),
        ),
        (
            "range_error_octet_index",
            string_octet("var answer = value[1];"),
        ),
        (
            "format_string_sprintf",
            caught("var value = \"%\"; var answer = value.sprintf();"),
        ),
        (
            "parameter_count_sprintf",
            caught("var value = \"%d\"; var answer = value.sprintf();"),
        ),
        (
            "parameter_count_char_at",
            caught("var value = \"abc\"; var answer = value.charAt();"),
        ),
        (
            "parameter_count_unpack",
            string_octet("var answer = value.unpack();"),
        ),
        (
            "invalid_parameter_unpack",
            string_octet("var answer = value.unpack(1);"),
        ),
    ]
}

/// A script that forces one refusal and answers the caught message.
fn caught(body: &str) -> String {
    format!("try {{ {body} }} catch(e) {{ return \"\" + e.message; }} return \"nocatch\";")
}

/// An octet literal holding `count` bytes of `01`.
fn octet_literal(count: usize) -> String {
    let mut text = String::from("<%");
    for _ in 0..count {
        text.push_str(" 01");
    }
    text.push_str(" %>");
    text
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
    eprintln!("raise_oracle: {message}");
    std::process::exit(1)
}
