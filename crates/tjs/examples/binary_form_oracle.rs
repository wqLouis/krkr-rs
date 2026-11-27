//! Records the bytes the engine writes for a value's binary structure, and the
//! value the same file reads back as.
//!
//! The surface under record is one save and one load: a one-item array holding
//! a value is written with `saveStruct(name, "b")`, the file's bytes are
//! recorded as hexadecimal, and the file is loaded back into an empty array
//! whose structured text is recorded beside the bytes. The frame is the array
//! because the save members belong to the container classes, not to a value.
//!
//! One line per row, `id<TAB>hex<TAB>text`: the id names the value, the hex is
//! the whole file — signature, frame and value — and the text is the loaded
//! array's text in the record's one-line form: a backslash is doubled, and a
//! carriage return, a line feed and a tab are written as `\r`, `\n` and `\t`.
//!
//! The record is written to `tests/fixtures/binary_form.txt` atomically: the
//! whole table is written to a temporary file beside the fixture and renamed
//! over it, so a failed run never leaves a half record behind. The temporary
//! files a row used are removed at the end of a successful run; a run that fails
//! leaves them where they are, under the system's temporary directory.
//!
//! ```sh
//! cargo run -p tjs --example binary_form_oracle
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the bytes the engine writes for a value's binary structure, and the value the file reads back as, recorded by `crates/tjs/examples/binary_form_oracle.rs`; regenerate with the command in the example's docs";

/// The signature the binary form starts with.
const SIGNATURE: [u8; 8] = *b"KBAD100\0";

/// The rows: the id the check names the row by, and the statements that build
/// the value the frame holds.
const ROWS: &[(&str, &str)] = &[
    ("integer_minus_one", "var __row = -1;"),
    ("integer_minus_32", "var __row = -32;"),
    ("integer_minus_33", "var __row = -33;"),
    ("integer_zero", "var __row = 0;"),
    ("integer_31", "var __row = 31;"),
    ("integer_32", "var __row = 32;"),
    ("integer_127", "var __row = 127;"),
    ("integer_128", "var __row = 128;"),
    ("integer_255", "var __row = 255;"),
    ("integer_256", "var __row = 256;"),
    ("integer_65535", "var __row = 65535;"),
    ("integer_65536", "var __row = 65536;"),
    ("real_half", "var __row = 3.5;"),
    ("text_five", "var __row = \"abcde\";"),
    ("text_sixteen", "var __row = \"0123456789abcdef\";"),
    ("array_empty", "var __row = [];"),
    (
        "dictionary_nested",
        "var __inner = new Dictionary(); __inner.k = [7]; var __row = __inner;",
    ),
    ("integer_minus_129", "var __row = -129;"),
    ("integer_minus_32769", "var __row = -32769;"),
    ("integer_4294967296", "var __row = 4294967296;"),
    (
        "text_thirty_two",
        "var __row = \"0123456789abcdefghijklmnopqrstuv\";",
    ),
    ("octet_three", "var __row = <% 00 AB FF %>;"),
    ("octet_six", "var __row = <% 01 02 03 04 05 06 %>;"),
    ("dictionary_empty", "var __row = new Dictionary();"),
    (
        "array_sixteen",
        "var __row = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];",
    ),
    ("void", "var __row = void;"),
    ("null", "var __row = null;"),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    let run = run_name();
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(id, build) in ROWS {
        let binary = temporary_path(&run, id, "bin");
        let script = format!(
            "{build}\nvar __frame = [__row];\n__frame.saveStruct({}, \"b\");",
            literal(&binary)
        );
        discard(&engine, &script, id);
        let written = read_bytes(&binary);
        if written.len() < SIGNATURE.len() || written[..SIGNATURE.len()] != SIGNATURE {
            fail(format_args!(
                "row {id}: the save did not write the signature"
            ));
        }
        discard(
            &engine,
            &format!(
                "var __back = [];\n__back.loadStruct({}, \"b\");\n__back.saveStruct({}, \"\");",
                literal(&binary),
                literal(&temporary_path(&run, id, "txt"))
            ),
            id,
        );
        let loaded = answer(&engine, "typeof (__back) + \"/\" + __back.count", id);
        let value = read_text(&temporary_path(&run, id, "txt"));
        remove(&binary, id);
        remove(&temporary_path(&run, id, "txt"), id);
        table.push_str(id);
        table.push('\t');
        table.push_str(&hex(&written));
        table.push('\t');
        table.push_str(&encode(&value));
        table.push('\n');
        println!(
            "binary_form_oracle: {id} wrote {} bytes, read back {loaded}",
            written.len()
        );
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
    println!("wrote {} ({} rows)", path.display(), ROWS.len());
}

/// Runs one statement script, which must compile and run.
fn discard(engine: &Tjs2Engine, script: &str, id: &str) {
    if let Err(error) = engine.exec_script_discard(script, "binary_form") {
        fail(format_args!(
            "row {id}: the engine rejected the script: {error}"
        ));
    }
}

/// The text of one evaluation, which must be a text.
fn answer(engine: &Tjs2Engine, expression: &str, id: &str) -> String {
    match engine.eval(expression, "binary_form") {
        Ok(TjsValue::String(text)) => text,
        Ok(other) => fail(format_args!(
            "row {id}: the answer is not a text: {other:?}"
        )),
        Err(error) => fail(format_args!("row {id}: the read was refused: {error}")),
    }
}

/// A path as the script literal that names it.
fn literal(path: &Path) -> String {
    format!("{:?}", path.to_string_lossy())
}

/// The prefix this run's temporary files carry, so two runs never share one.
fn run_name() -> String {
    let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_nanos(),
        Err(error) => fail(format_args!("the clock is not readable: {error}")),
    };
    format!("tjs-binary-form-{}-{nanos}", std::process::id())
}

/// The temporary file one row's save uses.
fn temporary_path(run: &str, id: &str, extension: &str) -> PathBuf {
    std::env::temp_dir().join(format!("{run}-{id}.{extension}"))
}

/// The bytes of one file.
fn read_bytes(path: &Path) -> Vec<u8> {
    match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => fail(format_args!("{} is not readable: {error}", path.display())),
    }
}

/// The bytes as the record spells them.
fn hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push_str(&format!("{byte:02X}"));
    }
    hex
}

/// The text of one save file: a byte-order mark and UTF-16 code units.
fn read_text(path: &Path) -> String {
    let bytes = read_bytes(path);
    if bytes.len() < 2 || bytes[0] != 0xff || bytes[1] != 0xfe {
        fail(format_args!(
            "{} is not the text stream a save writes",
            path.display()
        ));
    }
    let payload = &bytes[2..];
    if !payload.len().is_multiple_of(2) {
        fail(format_args!(
            "{} holds half a code unit at its end",
            path.display()
        ));
    }
    let mut units = Vec::with_capacity(payload.len() / 2);
    for pair in payload.as_chunks::<2>().0 {
        units.push(u16::from_le_bytes(*pair));
    }
    match String::from_utf16(&units) {
        Ok(text) => text,
        Err(error) => fail(format_args!("{} is not UTF-16: {error}", path.display())),
    }
}

/// Removes one row's temporary file.
fn remove(path: &Path, id: &str) {
    if let Err(error) = fs::remove_file(path) {
        fail(format_args!(
            "row {id}: the file {} is not removable: {error}",
            path.display()
        ));
    }
}

/// The record's one-line form of a text.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => encoded.push_str("\\\\"),
            '\r' => encoded.push_str("\\r"),
            '\n' => encoded.push_str("\\n"),
            '\t' => encoded.push_str("\\t"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/binary_form.txt")
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("binary_form_oracle: {message}");
    std::process::exit(1)
}
