//! The PSB scenario corpus: the game's PSB containers as TJS scripts.
//!
//! The game ships its scenarios as PSB containers (`*.ks.scn`). The
//! compatibility layer reads a container through the `psbfile` plugin, and
//! the plugin hands the script the container's root as TJS values. This
//! tool writes those values out as TJS source, one script per container, so
//! that the corpus holds the scenarios as scripts the front end compiles.
//!
//! The values are the plugin's own: the tool loads every container through
//! the plugin and renders the root the plugin built, so the script it writes
//! is the conversion the game sees. The engine's own loader reads the
//! archives, and the container's name is the one the archive holds it at.
//!
//! ```text
//! usage: psb_corpus <game directory> <output directory>
//! ```
//!
//! The output directory receives one `.tjs` file per container, in the
//! UTF-16 form the game's own scripts use.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use api::fs::FsPath;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use tjs::{MemberRead, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue};

/// The plugin that reads a container.
const PLUGIN: &str = "psbfile.dll";

/// The suffix a container's name carries.
const CONTAINER_SUFFIX: &str = ".scn";

/// The words the language reserves; a dictionary key that is one of them
/// cannot be written as a symbol.
const RESERVED: &[&str] = &[
    "break",
    "continue",
    "const",
    "catch",
    "class",
    "case",
    "debugger",
    "default",
    "delete",
    "do",
    "extends",
    "export",
    "enum",
    "else",
    "function",
    "finally",
    "false",
    "for",
    "global",
    "getter",
    "goto",
    "incontextof",
    "invalidate",
    "instanceof",
    "isvalid",
    "import",
    "int",
    "in",
    "if",
    "null",
    "new",
    "octet",
    "protected",
    "property",
    "private",
    "public",
    "return",
    "real",
    "synchronized",
    "switch",
    "static",
    "setter",
    "string",
    "super",
    "typeof",
    "throw",
    "this",
    "true",
    "try",
    "void",
    "var",
    "while",
    "NaN",
    "Infinity",
    "with",
];

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let Some(game) = arguments.next().map(PathBuf::from) else {
        eprintln!("usage: psb_corpus <game directory> <output directory>");
        return ExitCode::FAILURE;
    };
    let Some(output) = arguments.next().map(PathBuf::from) else {
        eprintln!("usage: psb_corpus <game directory> <output directory>");
        return ExitCode::FAILURE;
    };
    match run(&game, &output) {
        Ok(count) => {
            println!(
                "psb_corpus: {count} containers written to {}",
                output.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("psb_corpus: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Writes the script of every container the game holds.
fn run(game: &Path, output: &Path) -> Result<usize, String> {
    let file_system = FileSystem::host(game);
    let archives = engine::storage::discover_archives(&file_system, game)
        .map_err(|error| format!("the archives cannot be listed: {error}"))?;
    let mut storage = Storage::mount_archives(file_system, game.to_path_buf(), archives)
        .map_err(|error| format!("the archives cannot be mounted: {error}"))?;
    let containers = container_names(&storage);
    if containers.is_empty() {
        return Err("the game holds no container".to_string());
    }

    let engine = Tjs2Engine::new().map_err(|error| format!("the engine cannot start: {error}"))?;
    let mut host = CompatHost::new();
    // SAFETY: the storage is declared before the host, so it outlives the
    // host and the pointer the services hold stays valid.
    unsafe {
        host.services_mut().attach_storage(&mut storage);
    }
    host.install(&engine)
        .map_err(|error| format!("the surface cannot be installed: {error}"))?;
    host.activate_deferred_plugins()
        .map_err(|error| format!("the plugins cannot be activated: {error}"))?;
    engine
        .exec_script(&format!("Plugins.link(\"{PLUGIN}\");"), "psb_corpus")
        .map_err(|error| format!("the plugin cannot be linked: {error}"))?;

    let output_files = FileSystem::host(output);
    fs::create_dir_all(&output_files, &FsPath::host(output))
        .map_err(|error| format!("the output directory cannot be created: {error}"))?;

    let mut written = 0;
    for container in &containers {
        let root = engine
            .eval_retained(
                &format!("(new PSBFile({})).root", string_literal(container)),
                "psb_corpus",
            )
            .map_err(|error| format!("the container {container} cannot be read: {error}"))?;
        let mut script = String::new();
        render(&engine, &root, &mut script)?;
        script.push(';');
        let name = format!("{container}.tjs");
        let path = output.join(&name);
        let mut bytes = vec![0xff, 0xfe];
        for unit in script.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        fs::write(&output_files, &FsPath::host(&path), &bytes)
            .map_err(|error| format!("the script {name} cannot be written: {error}"))?;
        written += 1;
    }
    Ok(written)
}

/// The names of the containers the mounted archives hold.
fn container_names(storage: &Storage) -> Vec<String> {
    let mut names = Vec::new();
    storage.for_each_archive(|_path, archive| {
        for entry in archive.entries() {
            let name = entry.name.as_ref();
            if name.ends_with(CONTAINER_SUFFIX) {
                names.push(name.to_string());
            }
        }
    });
    names.sort();
    names.dedup();
    names
}

/// Writes the TJS source of a value the plugin built.
fn render(engine: &Tjs2Engine, value: &RetainedValue, out: &mut String) -> Result<(), String> {
    match value {
        RetainedValue::Object(object) => render_object(engine, object.raw_id(), out),
        RetainedValue::Value(value) => {
            render_scalar(value, out);
            Ok(())
        }
    }
}

/// Writes the TJS source of an object the plugin built.
fn render_object(engine: &Tjs2Engine, id: Tjs2ValueId, out: &mut String) -> Result<(), String> {
    if is_array(engine, id)? {
        render_array(engine, id, out)
    } else {
        render_dictionary(engine, id, out)
    }
}

/// Whether the object is an array.
fn is_array(engine: &Tjs2Engine, id: Tjs2ValueId) -> Result<bool, String> {
    let names = engine
        .class_names(id)
        .map_err(|error| format!("the class names cannot be read: {error}"))?;
    let count = count_of(engine, names.raw_id(), "the class names")?;
    for index in 0..count {
        let name = index.to_string();
        if let TjsValue::String(class) = engine.member(names.raw_id(), &name)?
            && class == "Array"
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Writes the TJS source of an array the plugin built.
fn render_array(engine: &Tjs2Engine, id: Tjs2ValueId, out: &mut String) -> Result<(), String> {
    let count = count_of(engine, id, "the array")?;
    out.push('[');
    for index in 0..count {
        if index != 0 {
            out.push(',');
        }
        let name = index.to_string();
        render_member(engine, id, &name, out)?;
    }
    out.push(']');
    Ok(())
}

/// Writes the TJS source of a dictionary the plugin built.
fn render_dictionary(engine: &Tjs2Engine, id: Tjs2ValueId, out: &mut String) -> Result<(), String> {
    let members = engine
        .enum_members(id)
        .map_err(|error| format!("the members cannot be listed: {error}"))?;
    out.push_str("%[");
    for (index, member) in members.iter().enumerate() {
        if index != 0 {
            out.push(',');
        }
        if is_symbol(member) {
            out.push_str(member);
            out.push(':');
        } else {
            // A key that is not a symbol is written as the pair form: the key
            // is an expression, and the value follows a comma.
            push_string(member, out);
            out.push(',');
        }
        render_member(engine, id, member, out)?;
    }
    out.push(']');
    Ok(())
}

/// Writes the TJS source of one member of an object.
fn render_member(
    engine: &Tjs2Engine,
    id: Tjs2ValueId,
    member: &str,
    out: &mut String,
) -> Result<(), String> {
    match engine
        .member_read(id, member)
        .map_err(|error| format!("the member {member} cannot be read: {error}"))?
    {
        MemberRead::Void => out.push_str("void"),
        MemberRead::Scalar(TjsValue::Void) => out.push_str("void"),
        MemberRead::Scalar(TjsValue::Integer(value)) => {
            let _ = write!(out, "{value}");
        }
        MemberRead::Scalar(TjsValue::Real(value)) => push_real(value, out),
        MemberRead::Scalar(TjsValue::String(value)) => push_string(&value, out),
        MemberRead::Scalar(other) => {
            return Err(format!(
                "the member {member} holds an unusable value: {other:?}"
            ));
        }
        MemberRead::Object(object) => render_object(engine, object.raw_id(), out)?,
        MemberRead::Octet(bytes) => push_octet(&bytes, out),
    }
    Ok(())
}

/// Writes a value that is not an object.
fn render_scalar(value: &TjsValue, out: &mut String) {
    match value {
        TjsValue::Void | TjsValue::Object => out.push_str("void"),
        TjsValue::Integer(number) => {
            let _ = write!(out, "{number}");
        }
        TjsValue::Real(number) => push_real(*number, out),
        TjsValue::String(text) => push_string(text, out),
        TjsValue::Retained(_) => out.push_str("void"),
    }
}

/// The number of elements an array holds.
fn count_of(engine: &Tjs2Engine, id: Tjs2ValueId, what: &str) -> Result<i64, String> {
    match engine.member(id, "count")? {
        TjsValue::Integer(value) => Ok(value),
        TjsValue::Real(value) => Ok(value as i64),
        other => Err(format!("{what} has no numeric count: {other:?}")),
    }
}

/// Whether a dictionary key can be written as a symbol.
fn is_symbol(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !characters.all(|character| character.is_ascii_alphanumeric() || character == '_') {
        return false;
    }
    !RESERVED.contains(&name)
}

/// Writes a text as a TJS string literal.
fn push_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\"\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if (character as u32) < 0x20 || character as u32 == 0x7f => {
                // The escape carries four digits, so the character after it
                // cannot join it.
                let _ = write!(out, "\\x{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out.push('"');
}

/// Writes a text as a TJS string literal, in its own text.
fn string_literal(text: &str) -> String {
    let mut out = String::new();
    push_string(text, &mut out);
    out
}

/// Writes a real as a TJS number literal.
fn push_real(value: f64, out: &mut String) {
    if value.is_nan() {
        out.push_str("NaN");
    } else if value.is_infinite() {
        out.push_str(if value.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        });
    } else {
        // The shortest form that reads back as the same real; a whole real
        // keeps its point, so it does not become an integer.
        let text = format!("{value:?}");
        out.push_str(&text);
        if !text.contains('.') && !text.contains('e') && !text.contains('E') {
            out.push_str(".0");
        }
    }
}

/// Writes bytes as a TJS octet literal.
fn push_octet(bytes: &[u8], out: &mut String) {
    out.push_str("<%");
    for byte in bytes {
        let _ = write!(out, " {byte:02x}");
    }
    out.push_str(" %>");
}
