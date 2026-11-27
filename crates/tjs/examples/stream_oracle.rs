//! Records what the engine answers for the members that reach a stream and for
//! the structured form both containers write.
//!
//! The surface under record is the five array members that talk to a stream
//! (`save`, `load`, `saveStruct`, `loadStruct`, `assignStruct`) and the
//! structured form a dictionary is written in. Every probe is a piece of
//! script the engine runs, and the answer is recorded verbatim: the text a
//! text save wrote, the bytes a binary save wrote, the value a load answered,
//! and the message a refused call raised.
//!
//! # The stream of a probe
//!
//! A member names its stream. This recorder runs the engine, whose streams are
//! files, so a probe's name is a file under the system's temporary directory
//! that only this run uses. The check replays the same probe against an
//! in-memory host keyed by the same name, so no name crosses the record.
//!
//! A text save writes the UTF-16 text behind the byte-order mark the stream
//! owns; the record holds the text without the mark, escaped so a probe stays
//! on one line: a backslash is doubled, and a carriage return, a line feed and
//! a tab are written as `\r`, `\n` and `\t`. A binary save's record holds the
//! file's bytes as uppercase hexadecimal.
//!
//! # What a probe records
//!
//! One line per probe, `probe<TAB>kind<TAB>text`, where the kind says what the
//! text is:
//!
//! | kind | text |
//! |---|---|
//! | `text` | a text save, escaped |
//! | `bytes` | a binary save, hexadecimal |
//! | `!raise` | the message a refused call raised |
//! | `answer` | a call that raised nothing where a probe expected a raise |
//! | a `typeof` spelling | the answer the language gave, as text |
//!
//! The record is built whole and written beside the fixture, then renamed over
//! it, so a run that fails leaves the old record in place. The temporary files
//! a probe wrote are removed at the end of a successful run; a run that fails
//! leaves them where they are, under the system's temporary directory.
//!
//! ```sh
//! cargo run -p tjs --example stream_oracle
//! ```
//!
//! The record goes to the committed fixture; an argument names another path.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/stream_oracle.rs`; regenerate with the command in the example's docs";

/// The name no host serves, for the probes that ask what a refused open
/// answers. The recorder and the check hand the same name over, so the message
/// the record holds is the same on both sides.
const ABSENT: &str = "@tjs-stream-fixture-absent";

/// The script that builds every container a probe reads.
///
/// The builders are named functions because a script cannot declare one inside
/// an anonymous body, and they are deleted after the call so the global object
/// is left as it was. The helpers below answer one value each: a probe's answer
/// is the value a probe table can compare, never an object, whose text form is
/// its address.
const SETUP: &str = r#"function streamShow() {
    var s = new Array();
    s[0] = "one";
    s[1] = 2;
    s[2] = -9;
    s[3] = 3.5;
    s[4] = 1234567890123;
    s[5] = <% 00 AB %>;
    s[6] = void;
    s[7] = null;
    s[8] = "a,b\r\n\"q\"";
    var nested = new Array();
    nested[0] = 1;
    nested[1] = "two";
    var deep = new Dictionary();
    deep.k = 1;
    nested[2] = deep;
    s[9] = nested;
    var map = new Dictionary();
    map.key = "value";
    map.num = 7;
    s[10] = map;
    s[11] = [];
    s[12] = %[];
    return s;
}
function streamSaved() {
    var d = new Dictionary();
    d.integer = 42;
    d.text = "a,b\r\n\"q\"";
    var nested = new Dictionary();
    nested.inner = 1;
    d.nested = nested;
    var list = new Array();
    list[0] = 1;
    list[1] = 2;
    d.list = list;
    d.empty = %[];
    d.self = d;
    return d;
}
function streamPlain() {
    var p = new Dictionary();
    p.name = "plain";
    p.count = 3;
    p.real = 0.5;
    p.octet = <% 01 02 %>;
    var list = new Array();
    list[0] = 1;
    list[1] = "two";
    p.list = list;
    var map = new Dictionary();
    map.deep = 1;
    p.map = map;
    return p;
}
var show = streamShow();
var saved = streamSaved();
var plain = streamPlain();
var tiny = ["abcdef"];
delete this.streamShow;
delete this.streamSaved;
delete this.streamPlain;
function workArray() {
    return new Array();
}
function linesCount(name) {
    var w = new Array();
    w.load(name, "");
    return w.count;
}
function linesAt(name, index) {
    var w = new Array();
    w.load(name, "");
    return w[index];
}
function linesResave(name, out) {
    var w = new Array();
    w.load(name, "");
    w.save(out, "");
}
function structCount(name) {
    var w = new Array();
    w.loadStruct(name, "");
    return w.count;
}
function structAt(name, index) {
    var w = new Array();
    w.loadStruct(name, "");
    return w[index];
}
function structType(name, index) {
    var w = new Array();
    w.loadStruct(name, "");
    return typeof (w[index]);
}
function structResaveBinary(name, out) {
    var w = new Array();
    w.loadStruct(name, "");
    w.saveStruct(out, "b");
}
function savedFrame(out) {
    var f = [saved];
    f.saveStruct(out, "");
}
function plainFrameText(out) {
    var f = [plain];
    f.saveStruct(out, "");
}
function plainFrameBinary(out) {
    var f = [plain];
    f.saveStruct(out, "b");
}
function plainFrameReload(name, out) {
    var w = new Array();
    w.loadStruct(name, "");
    var f = [w[0]];
    f.saveStruct(out, "b");
}
function copyBinary(out) {
    var src = [1, "two", 3.5, [4, 5]];
    var dst = new Array();
    dst.assignStruct(src);
    dst.saveStruct(out, "b");
}
function tinyBinary(out) {
    tiny.saveStruct(out, "b");
}
function saveAnswerKind(name) {
    return typeof (show.save(name, ""));
}
function saveAnswerSelf(name) {
    return (show.save(name, "") === show) ? 1 : 0;
}
function saveStructAnswerKind(name) {
    return typeof (show.saveStruct(name, ""));
}
function saveStructAnswerSelf(name) {
    return (show.saveStruct(name, "") === show) ? 1 : 0;
}
function loadAnswerKind(name) {
    var w = new Array();
    return typeof (w.load(name, ""));
}
function loadAnswerSelf(name) {
    var w = new Array();
    return (w.load(name, "") === w) ? 1 : 0;
}
function loadStructAnswerKind(name) {
    var w = new Array();
    return typeof (w.loadStruct(name, ""));
}
function loadStructAnswerSelf(name) {
    var w = new Array();
    return (w.loadStruct(name, "") === w) ? 1 : 0;
}
function assignAnswerKind() {
    var w = new Array();
    return typeof (w.assignStruct([1]));
}
"#;

/// The form a save writes.
#[derive(Clone, Copy, Debug)]
enum Form {
    /// A text save: the record holds the escaped text.
    Text,
    /// A binary save: the record holds the bytes as hexadecimal.
    Bytes,
}

/// One probe, and what it asks.
///
/// A `{name}` in a script is the file that name stands for, replaced before the
/// script is handed to the engine; `{absent}` is the name no host serves.
enum Step {
    /// A script whose answer is recorded: the kind the language names it by,
    /// and its text.
    Answer {
        /// The id the check names the rule by.
        probe: &'static str,
        /// The expression the answer is read from.
        expression: &'static str,
    },
    /// A script that writes a stream, and the file the stream is.
    Saved {
        /// The id the check names the rule by.
        probe: &'static str,
        /// The statement that saves.
        script: &'static str,
        /// The name the save writes.
        file: &'static str,
        /// The form the save writes.
        form: Form,
    },
    /// A text save whose text is read back as the expression it is, and saved
    /// again: the round trip the text form claims.
    Reload {
        /// The id the check names the rule by.
        probe: &'static str,
        /// The text the first save wrote.
        from: &'static str,
        /// The text the second save writes.
        to: &'static str,
    },
    /// A call a rule must refuse; the refusal's own message is recorded.
    Raised {
        /// The id the check names the rule by.
        probe: &'static str,
        /// The call that is refused.
        call: &'static str,
    },
    /// The file `to` as the file `from` with its last byte removed.
    Truncated {
        /// The file a save wrote.
        from: &'static str,
        /// The file the truncated bytes are written to.
        to: &'static str,
    },
}

/// The probes, in the order the record holds them.
///
/// A save comes before the probe that reads it. The names are the names the
/// check keys its in-memory host by; a name is not a path here.
const STEPS: &[Step] = &[
    // the structured text form of an array
    Step::Saved {
        probe: "save_show_text",
        script: "show.saveStruct({show.txt}, \"\")",
        file: "show.txt",
        form: Form::Text,
    },
    Step::Reload {
        probe: "save_show_text_reloaded",
        from: "show.txt",
        to: "show-again.txt",
    },
    // the line form
    Step::Saved {
        probe: "save_show_lines",
        script: "show.save({lines.txt}, \"\")",
        file: "lines.txt",
        form: Form::Text,
    },
    Step::Answer {
        probe: "load_lines_count",
        expression: "linesCount({lines.txt})",
    },
    Step::Answer {
        probe: "load_lines_first",
        expression: "linesAt({lines.txt}, 0)",
    },
    Step::Answer {
        probe: "load_lines_real",
        expression: "linesAt({lines.txt}, 3)",
    },
    Step::Answer {
        probe: "load_lines_octet",
        expression: "linesAt({lines.txt}, 5)",
    },
    Step::Saved {
        probe: "save_lines_again",
        script: "linesResave({lines.txt}, {lines-again.txt})",
        file: "lines-again.txt",
        form: Form::Text,
    },
    // the binary form
    Step::Saved {
        probe: "save_show_binary",
        script: "show.saveStruct({show.bin}, \"b\")",
        file: "show.bin",
        form: Form::Bytes,
    },
    Step::Answer {
        probe: "load_struct_bin_count",
        expression: "structCount({show.bin})",
    },
    Step::Answer {
        probe: "load_struct_bin_huge",
        expression: "structAt({show.bin}, 4)",
    },
    Step::Answer {
        probe: "load_struct_bin_real",
        expression: "structAt({show.bin}, 3)",
    },
    Step::Answer {
        probe: "load_struct_bin_real_kind",
        expression: "structType({show.bin}, 3)",
    },
    Step::Answer {
        probe: "load_struct_bin_octet",
        expression: "structType({show.bin}, 5)",
    },
    Step::Answer {
        probe: "load_struct_bin_null",
        expression: "structType({show.bin}, 7)",
    },
    Step::Saved {
        probe: "save_struct_bin_again",
        script: "structResaveBinary({show.bin}, {show-again.bin})",
        file: "show-again.bin",
        form: Form::Bytes,
    },
    // the dictionary's structure, through the frame a script reaches it with
    Step::Saved {
        probe: "save_saved_frame",
        script: "savedFrame({saved.txt})",
        file: "saved.txt",
        form: Form::Text,
    },
    Step::Saved {
        probe: "save_plain_frame",
        script: "plainFrameText({plain.txt})",
        file: "plain.txt",
        form: Form::Text,
    },
    Step::Saved {
        probe: "save_plain_frame_binary",
        script: "plainFrameBinary({plain.bin})",
        file: "plain.bin",
        form: Form::Bytes,
    },
    Step::Saved {
        probe: "save_plain_reload",
        script: "plainFrameReload({plain.bin}, {plain-again.bin})",
        file: "plain-again.bin",
        form: Form::Bytes,
    },
    Step::Saved {
        probe: "save_copy_binary",
        script: "copyBinary({copy.bin})",
        file: "copy.bin",
        form: Form::Bytes,
    },
    Step::Saved {
        probe: "save_tiny_binary",
        script: "tinyBinary({tiny.bin})",
        file: "tiny.bin",
        form: Form::Bytes,
    },
    Step::Truncated {
        from: "tiny.bin",
        to: "tiny-cut.bin",
    },
    // the refusals
    Step::Raised {
        probe: "load_absent",
        call: "workArray().load({absent}, \"\")",
    },
    Step::Raised {
        probe: "load_struct_absent",
        call: "workArray().loadStruct({absent}, \"\")",
    },
    Step::Raised {
        probe: "assign_struct_mismatch",
        call: "workArray().assignStruct(saved)",
    },
    Step::Raised {
        probe: "assign_struct_null",
        call: "workArray().assignStruct(null)",
    },
    Step::Raised {
        probe: "load_struct_not_binary",
        call: "workArray().loadStruct({show.txt}, \"\")",
    },
    Step::Raised {
        probe: "load_struct_truncated",
        call: "workArray().loadStruct({tiny-cut.bin}, \"\")",
    },
    // the member's own answer
    Step::Answer {
        probe: "save_answer_kind",
        expression: "saveAnswerKind({show.txt})",
    },
    Step::Answer {
        probe: "save_answer_self",
        expression: "saveAnswerSelf({show.txt})",
    },
    Step::Answer {
        probe: "save_struct_answer_kind",
        expression: "saveStructAnswerKind({show.txt})",
    },
    Step::Answer {
        probe: "save_struct_answer_self",
        expression: "saveStructAnswerSelf({show.txt})",
    },
    Step::Answer {
        probe: "load_answer_kind",
        expression: "loadAnswerKind({lines.txt})",
    },
    Step::Answer {
        probe: "load_answer_self",
        expression: "loadAnswerSelf({lines.txt})",
    },
    Step::Answer {
        probe: "load_struct_answer_kind",
        expression: "loadStructAnswerKind({show.bin})",
    },
    Step::Answer {
        probe: "load_struct_answer_self",
        expression: "loadStructAnswerSelf({show.bin})",
    },
    Step::Answer {
        probe: "assign_answer_kind",
        expression: "assignAnswerKind()",
    },
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "stream_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let files = Files::new();
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for step in STEPS {
        run(&engine, &files, &mut table, step);
    }
    files.remove();
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

/// Runs one probe and appends its record line.
fn run(engine: &Tjs2Engine, files: &Files, table: &mut String, step: &Step) {
    match step {
        Step::Answer { probe, expression } => {
            let script = format!("var __answer = {};", resolve(expression, files));
            discard(engine, &script, probe);
            let kind = text(engine, "typeof (__answer)", probe);
            if kind == "Object" {
                fail(format_args!(
                    "probe {probe}: the answer is an object, whose text form is its \
                     address; the probe must answer a value the record can compare"
                ));
            }
            let answer = text(engine, "\"\" + __answer", probe);
            row(table, probe, &kind, &encode(&answer));
        }
        Step::Saved {
            probe,
            script,
            file,
            form,
        } => {
            let script = format!("{};", resolve(script, files));
            discard(engine, &script, probe);
            let path = files.path(file);
            match form {
                Form::Text => {
                    let written = read_text(&path);
                    row(table, probe, "text", &encode(&written));
                }
                Form::Bytes => {
                    let written = read_bytes(&path);
                    row(table, probe, "bytes", &hex(&written));
                }
            }
        }
        Step::Reload { probe, from, to } => {
            // The text is bound and saved by one script: a name the script
            // assigns without declaring is not the name the statement after it
            // reads, while a top-level `var` is.
            let loaded = read_text(&files.path(from));
            let script = format!(
                "var __reloaded = {loaded};\n__reloaded.saveStruct({}, \"\");",
                literal(&files.path(to))
            );
            discard(engine, &script, probe);
            let written = read_text(&files.path(to));
            row(table, probe, "text", &encode(&written));
        }
        Step::Raised { probe, call } => {
            let script = format!(
                "var __raised = \"\";\ntry {{ {}; }} catch(__error) {{ __raised = __error.message; }}",
                resolve(call, files)
            );
            discard(engine, &script, probe);
            let message = text(engine, "__raised", probe);
            if message.is_empty() {
                row(table, probe, "answer", "(no raise)");
                println!("stream_oracle: probe {probe} answered instead of raising");
            } else {
                row(table, probe, "!raise", &encode(&message));
            }
        }
        Step::Truncated { from, to } => {
            let bytes = read_bytes(&files.path(from));
            let Some((_last, rest)) = bytes.split_last() else {
                fail(format_args!(
                    "the file {} holds no byte to remove",
                    files.path(from).display()
                ));
            };
            if let Err(error) = fs::write(files.path(to), rest) {
                fail(format_args!(
                    "the truncated file {} is not writable: {error}",
                    files.path(to).display()
                ));
            }
        }
    }
}

/// The script with every `{name}` spelled as the literal path that name stands
/// for.
fn resolve(script: &str, files: &Files) -> String {
    let mut resolved = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(open) = rest.find('{') {
        resolved.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            fail(format_args!("the placeholder in {script:?} is not closed"));
        };
        let key = &after[..close];
        if key == "absent" {
            resolved.push_str(&literal(Path::new(ABSENT)));
        } else {
            resolved.push_str(&literal(&files.path(key)));
        }
        rest = &after[close + 1..];
    }
    resolved.push_str(rest);
    resolved
}

/// A path as the script literal that names it.
fn literal(path: &Path) -> String {
    format!("{:?}", path.to_string_lossy())
}

/// The text of one evaluation, which must be a text.
fn text(engine: &Tjs2Engine, expression: &str, probe: &str) -> String {
    match engine.eval(expression, "stream_members") {
        Ok(TjsValue::String(text)) => text,
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(error) => fail(format_args!(
            "probe {probe}: the expression was refused: {error}"
        )),
    }
}

/// Runs one statement script, which must compile and run.
fn discard(engine: &Tjs2Engine, script: &str, probe: &str) {
    if let Err(error) = engine.exec_script_discard(script, "stream_members") {
        fail(format_args!(
            "probe {probe}: the engine rejected the script: {error}"
        ));
    }
}

/// Appends one record line.
fn row(table: &mut String, probe: &str, kind: &str, text: &str) {
    table.push_str(probe);
    table.push('\t');
    table.push_str(kind);
    table.push('\t');
    table.push_str(text);
    table.push('\n');
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
    match std::env::args_os().nth(1) {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stream_members.txt"),
    }
}

/// The temporary directory this run's streams are written to.
///
/// The directory is unique to the run, so two runs cannot read each other's
/// streams, and it holds no name the record carries.
struct Files {
    /// The directory every stream is a file in.
    directory: PathBuf,
}

impl Files {
    /// A directory no other run of the recorder is using.
    fn new() -> Self {
        let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_nanos(),
            Err(error) => fail(format_args!("the clock is not readable: {error}")),
        };
        let directory =
            std::env::temp_dir().join(format!("tjs-stream-{}-{nanos}", std::process::id()));
        if let Err(error) = fs::create_dir_all(&directory) {
            fail(format_args!(
                "the temporary directory {} is not creatable: {error}",
                directory.display()
            ));
        }
        Self { directory }
    }

    /// The file a name stands for.
    fn path(&self, name: &str) -> PathBuf {
        self.directory.join(name)
    }

    /// Removes the files this run wrote and the directory that held them.
    fn remove(&self) {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) => fail(format_args!(
                "the temporary directory {} is not readable: {error}",
                self.directory.display()
            )),
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => fail(format_args!(
                    "the temporary directory {} is not readable: {error}",
                    self.directory.display()
                )),
            };
            remove_if_present(&entry.path());
        }
        if let Err(error) = fs::remove_dir(&self.directory) {
            fail(format_args!(
                "the temporary directory {} is not removable: {error}",
                self.directory.display()
            ));
        }
    }
}

/// Removes one file, which has to be gone or already absent.
fn remove_if_present(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => fail(format_args!("{} is not removable: {error}", path.display())),
    }
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("stream_oracle: {message}");
    std::process::exit(1)
}
