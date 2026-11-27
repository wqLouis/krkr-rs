//! Records the engine's messages and what an assignment changes.
//!
//! The script probes force one refusal inside a `try` and answer `e.message`
//! from the catch. The assignment probes use `Tjs2Engine::assign_message` and
//! then raise again; one probe saves the exception from a raise, reassigns the
//! message, and reads the saved exception's message, so the record says whether
//! an assignment reaches an exception that was already made.
//!
//! The record is written to `tests/fixtures/message_table.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example message_oracle
//! ```

use std::fs;
use std::path::PathBuf;

use tjs::Tjs2Engine;
use tjs::TjsValue;
use tjs::vm::runtime::exec::Raise;
use tjs::vm::runtime::exec::throw::{Unwind, message_of};
use tjs::vm::runtime::message;

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/message_oracle.rs`; regenerate with the command in the example's docs";

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    message::reset();
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');

    // The default text of every id this tree raises. The rows run before any
    // assignment, so each one reads its default.
    let defaults: &[(&str, String)] = &[
        (
            "default_variant_convert",
            caught(
                &engine,
                "var value = null; var answer = (int)value;",
                "default_variant_convert",
            ),
        ),
        (
            "default_variant_convert_to_object",
            caught(
                &engine,
                "var value = 1; var answer = value();",
                "default_variant_convert_to_object",
            ),
        ),
        (
            "default_divide_by_zero",
            caught(
                &engine,
                r"var one = 1; var zero = 0; var value = one \ zero;",
                "default_divide_by_zero",
            ),
        ),
        (
            "default_integer_out_of_range",
            message_of(&Unwind::raised(Raise::IntegerOutOfRange)),
        ),
        (
            "default_member_not_found",
            caught(&engine, "var answer = absent;", "default_member_not_found"),
        ),
        (
            "default_access_denied",
            caught(
                &engine,
                "var value = \"abc\"; value.length = 1;",
                "default_access_denied",
            ),
        ),
        (
            "default_range_error",
            caught(
                &engine,
                "var value = \"abc\"; var answer = value[4];",
                "default_range_error",
            ),
        ),
        (
            "default_not_callable",
            caught(
                &engine,
                "var value = (const) %[]; value.f = 1; var answer = value.f();",
                "default_not_callable",
            ),
        ),
        (
            "default_parameter_count",
            caught(
                &engine,
                "var value = \"abc\"; var answer = value.charAt();",
                "default_parameter_count",
            ),
        ),
        (
            "default_invalid_parameter",
            caught(
                &engine,
                "var value = <% 01 %>; var answer = value.unpack(1);",
                "default_invalid_parameter",
            ),
        ),
        (
            "default_format_string",
            caught(
                &engine,
                "var value = \"%\"; var answer = value.sprintf();",
                "default_format_string",
            ),
        ),
        (
            "default_not_an_array",
            message_of(&Unwind::raised(Raise::NotAnArray)),
        ),
        (
            "default_not_implemented",
            caught(
                &engine,
                "var value = (const) [1, 2, 3]; var answer = value.count();",
                "default_not_implemented",
            ),
        ),
    ];
    for (probe, answer) in defaults {
        push(&mut table, probe, answer.as_str());
    }

    // A later raise of an assigned id reads the assigned text.
    push(
        &mut table,
        "assign_known",
        &assign(
            &engine,
            "TJSBadParamCount",
            "the assigned count",
            "assign_known",
        ),
    );
    push(
        &mut table,
        "later_raise",
        &caught(
            &engine,
            "var value = \"abc\"; var answer = value.charAt();",
            "later_raise",
        ),
    );

    // An exception that was already made keeps the text it was made with.
    run(
        &engine,
        r#"var messageTableSaved = void;
try {
    var value = "abc";
    var answer = value.charAt();
} catch (e) {
    messageTableSaved = e;
}"#,
        "saved_raise",
    );
    push(
        &mut table,
        "saved_raise",
        &text(
            &engine,
            "return \"\" + messageTableSaved.message;",
            "saved_raise",
        ),
    );
    push(
        &mut table,
        "assign_replacement",
        &assign(
            &engine,
            "TJSBadParamCount",
            "the second count",
            "assign_replacement",
        ),
    );
    push(
        &mut table,
        "already_raised",
        &text(
            &engine,
            "return \"\" + messageTableSaved.message;",
            "already_raised",
        ),
    );
    push(
        &mut table,
        "later_raise_after_replacement",
        &caught(
            &engine,
            "var value = \"abc\"; var answer = value.charAt();",
            "later_raise_after_replacement",
        ),
    );

    // An unknown id is not stored.
    push(
        &mut table,
        "assign_unknown",
        &assign(&engine, "not-a-real-message", "x", "assign_unknown"),
    );

    // A registered id outside the raise paths is assignable too.
    push(
        &mut table,
        "assign_internal_error",
        &assign(
            &engine,
            "TJSInternalError",
            "hello",
            "assign_internal_error",
        ),
    );

    // The assigned text keeps its placeholders, and the raise substitutes them.
    push(
        &mut table,
        "assign_with_placeholder",
        &assign(
            &engine,
            "TJSVariantConvertError",
            "changed %1 to %2",
            "assign_with_placeholder",
        ),
    );
    push(
        &mut table,
        "later_raise_with_placeholder",
        &caught(
            &engine,
            "var value = null; var answer = (int)value;",
            "later_raise_with_placeholder",
        ),
    );

    // The declaration failure's message is a registered id, so an assignment
    // reaches the raise.
    push(
        &mut table,
        "assign_not_implemented",
        &assign(
            &engine,
            "TJSNotImplemented",
            "the assigned method",
            "assign_not_implemented",
        ),
    );
    push(
        &mut table,
        "later_raise_not_implemented",
        &caught(
            &engine,
            "var value = (const) [1, 2, 3]; var answer = value.count();",
            "later_raise_not_implemented",
        ),
    );

    // The tree's own integer-range message has no script path in the engine,
    // so the assignment and the read are the Rust table's.
    let assigned = message::assign(message::TJS_INTEGER_OUT_OF_RANGE, "the assigned integer");
    push(
        &mut table,
        "assign_integer_out_of_range",
        &bool_text(assigned),
    );
    push(
        &mut table,
        "later_raise_integer_out_of_range",
        &message_of(&Unwind::raised(Raise::IntegerOutOfRange)),
    );

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/message_table.txt")
}

/// Appends one `probe<TAB>message` row.
fn push(table: &mut String, probe: &str, message: &str) {
    table.push_str(probe);
    table.push('\t');
    table.push_str(&escaped(message));
    table.push('\n');
}

/// Assigns `id` and answers the success flag as text.
fn assign(engine: &Tjs2Engine, id: &str, text: &str, probe: &str) -> String {
    match engine.assign_message(id, text) {
        Ok(assigned) => bool_text(assigned),
        Err(error) => fail(format_args!(
            "probe {probe}: the assignment failed: {error}"
        )),
    }
}

/// The text a boolean answers in the record.
fn bool_text(value: bool) -> String {
    if value {
        String::from("1")
    } else {
        String::from("0")
    }
}

/// A script that forces one refusal and answers the caught message.
fn caught(engine: &Tjs2Engine, body: &str, probe: &str) -> String {
    let script =
        format!("try {{ {body} }} catch (e) {{ return \"\" + e.message; }} return \"nocatch\";");
    text(engine, &script, probe)
}

/// Runs a script and answers the text it returned.
fn text(engine: &Tjs2Engine, script: &str, probe: &str) -> String {
    match engine.exec_script(script, probe) {
        Ok(TjsValue::String(text)) => text,
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(error) => fail(format_args!("probe {probe}: the script failed: {error}")),
    }
}

/// Runs a script whose answer the next probe reads from a global.
fn run(engine: &Tjs2Engine, script: &str, probe: &str) {
    if let Err(error) = engine.exec_script_discard(script, probe) {
        fail(format_args!("probe {probe}: the script failed: {error}"));
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
    eprintln!("message_oracle: {message}");
    std::process::exit(1)
}
