//! The corpus digest: the check that the front end's compiled script
//! blocks have not changed.
//!
//! The corpus is the set of scripts a game ships. It is not in this
//! repository — the game is not either — so the corpus is extracted from
//! the game's archives with the crate's own `tools/extract_corpus.py`,
//! and the check is run against the extracted corpus.
//!
//! The corpus holds two kinds of script. The **shipped** scripts are the
//! `.tjs` files the game's archives carry. The **generated** scripts are the
//! game's PSB scenarios: the compatibility layer converts a container into
//! TJS, and the corpus holds that conversion under the
//! [`GENERATED_DIRECTORY`]. The two kinds are compared the same way; only
//! the directory they live in differs.
//!
//! The fixture is the digest of every script of the corpus, one line per
//! script: the digest, and the script's path under the corpus root. The
//! digests were recorded from the front end this one replaces, so they
//! are the record of what that front end produced; this check compares
//! what this front end produces with them.
//!
//! The digest covers the whole compiled script block: every context's
//! code stream, constant area, counts, source positions, relations and
//! member registrations. It is a digest of the result, not of the way
//! the result is carried, so the two front ends agree on it.
//!
//! # The run
//!
//! ```text
//! cargo run -p tjs --example corpus_digest -- --corpus <corpus directory>
//! ```
//!
//! The check fails when the corpus is missing, when a digest differs and
//! when the corpus and the fixture do not hold the same scripts. It does
//! not report success in any of those cases.
//!
//! # The options
//!
//! * `--corpus <directory>` — the directory the corpus was extracted
//!   into. The check fails when it is missing.
//! * `--fixture <file>` — the fixture the check reads. The default is the
//!   crate's own `tests/fixtures/corpus_digest.txt`.
//! * `--record <file>` — writes the digests this front end produces to a
//!   file, so that they can be compared with the fixture's.
//! * `--canonical <script>` — writes the canonical form of one script to
//!   the standard output, so that two front ends can be compared by
//!   hand.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tjs::vm::codegen::context::ContextType;
use tjs::vm::codegen::value::Value;
use tjs::vm::{Generator, Lexer, Parser};

/// The fixture the check reads when no other one is named.
const FIXTURE: &str = "tests/fixtures/corpus_digest.txt";

/// The directory under the corpus root that holds the generated scripts.
///
/// The game's PSB scenarios are converted into TJS by the compatibility layer
/// and written here by the crate's own producer
/// (`crates/compat/examples/psb_corpus.rs`). A generated script the fixture
/// holds no digest for is **not recorded yet**: the digests are recorded from
/// the front end this one replaces, and that recording is a step of its own.
/// The check reports those scripts and compares them as soon as the fixture
/// holds their digests; it does not fail on them, because a script nobody
/// has recorded is not a difference.
const GENERATED_DIRECTORY: &str = "psb";

/// The options the run was given.
struct Options {
    /// The directory the corpus was extracted into.
    corpus: Option<PathBuf>,
    /// The fixture the check reads.
    fixture: PathBuf,
    /// The file the digests are recorded in.
    record: Option<PathBuf>,
    /// The script whose canonical form is written out.
    canonical: Option<PathBuf>,
}

/// The fixture's records: the digest of every script, and the scripts
/// that are not comparable.
struct Fixture {
    /// The digest of every comparable script.
    digests: BTreeMap<String, String>,
    /// The scripts that are not comparable.
    skipped: BTreeSet<String>,
}

/// The result of a run: what the check found.
struct Report {
    /// The scripts the check compared.
    compared: usize,
    /// The scripts whose digest differs from the fixture's.
    different: Vec<String>,
    /// The scripts the fixture holds and the corpus does not.
    missing: Vec<String>,
    /// The scripts the corpus holds and the fixture does not.
    unknown: Vec<String>,
    /// The generated scripts the fixture holds no digest for yet.
    pending: Vec<String>,
}

fn main() -> ExitCode {
    let options = match read_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    if let Some(script) = options.canonical.as_ref() {
        return match canonical_of_file(script) {
            Ok(form) => {
                print!("{form}");
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("{message}");
                ExitCode::FAILURE
            }
        };
    }

    let corpus = match options.corpus.as_ref() {
        Some(corpus) => corpus.clone(),
        None => {
            eprintln!(
                "the corpus is not named: the check needs the directory the \
                 corpus was extracted into (--corpus <directory>)"
            );
            return ExitCode::FAILURE;
        }
    };
    if !corpus.is_dir() {
        eprintln!(
            "the corpus is missing: {} is not a directory.\n\
             The corpus is not in this repository. Extract it from the game \
             with the crate's own script:\n\
             \n    python3 crates/tjs/tools/extract_corpus.py <game \
             directory> <corpus directory>\n",
            corpus.display()
        );
        return ExitCode::FAILURE;
    }

    let fixture = match read_fixture(&options.fixture) {
        Ok(fixture) => fixture,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let scripts = match corpus_scripts(&corpus) {
        Ok(scripts) => scripts,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
    };

    let mut recorded: Vec<String> = Vec::new();
    let mut report = Report {
        compared: 0,
        different: Vec::new(),
        missing: Vec::new(),
        unknown: Vec::new(),
        pending: Vec::new(),
    };

    for (relative, path) in &scripts {
        let Some(expected) = fixture.digests.get(relative) else {
            if !fixture.skipped.contains(relative) {
                if is_generated(relative) {
                    report.pending.push(relative.clone());
                } else {
                    report.unknown.push(relative.clone());
                }
            }
            continue;
        };
        let form = match canonical_of_file(path) {
            Ok(form) => form,
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::FAILURE;
            }
        };
        let digest = sha256_hex(form.as_bytes());
        recorded.push(format!("{digest}  {relative}"));
        report.compared += 1;
        if digest != *expected {
            report.different.push(relative.clone());
            eprintln!("the digest differs: {relative}");
            eprintln!("  the fixture: {expected}");
            eprintln!("  this front end: {digest}");
        }
    }

    for relative in fixture.digests.keys().chain(fixture.skipped.iter()) {
        if !scripts.iter().any(|(other, _)| other == relative) {
            report.missing.push(relative.clone());
        }
    }

    if let Some(path) = options.record.as_ref() {
        if let Err(message) = write_record(path, &recorded) {
            eprintln!("{message}");
            return ExitCode::FAILURE;
        }
        println!("the digests are in {}", path.display());
    }

    println!(
        "the corpus: {} scripts, {} compared, {} skipped, {} not recorded yet",
        scripts.len(),
        report.compared,
        fixture.skipped.len(),
        report.pending.len()
    );
    if !report.pending.is_empty() {
        println!(
            "the fixture holds no digest for {} generated scripts:",
            report.pending.len()
        );
        for relative in &report.pending {
            println!("  {relative}");
        }
    }
    if !report.unknown.is_empty() {
        println!(
            "the fixture holds no digest for {} scripts:",
            report.unknown.len()
        );
        for relative in &report.unknown {
            println!("  {relative}");
        }
    }
    if !report.missing.is_empty() {
        println!(
            "the corpus holds no script for {} of them:",
            report.missing.len()
        );
        for relative in &report.missing {
            println!("  {relative}");
        }
    }

    if report.different.is_empty() && report.unknown.is_empty() && report.missing.is_empty() {
        println!("every digest is the fixture's");
        return ExitCode::SUCCESS;
    }
    println!(
        "{} digests differ, {} scripts are not in the fixture, {} are missing \
         from the corpus",
        report.different.len(),
        report.unknown.len(),
        report.missing.len()
    );
    ExitCode::FAILURE
}

/// Reads the options the run was given.
fn read_options() -> Result<Options, String> {
    let mut corpus = None;
    let mut fixture = None;
    let mut record = None;
    let mut canonical = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .ok_or_else(|| format!("{argument} needs a value"))
        };
        match argument.as_str() {
            "--corpus" => corpus = Some(PathBuf::from(value()?)),
            "--fixture" => fixture = Some(PathBuf::from(value()?)),
            "--record" => record = Some(PathBuf::from(value()?)),
            "--canonical" => canonical = Some(PathBuf::from(value()?)),
            "--help" | "-h" => {
                println!(
                    "usage: corpus_digest --corpus <directory> [--fixture <file>] \
                     [--record <file>] [--canonical <script>]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("{other} is not an option")),
        }
    }
    Ok(Options {
        corpus,
        fixture: fixture.unwrap_or_else(default_fixture),
        record,
        canonical,
    })
}

/// The fixture the crate ships.
fn default_fixture() -> PathBuf {
    let root = option_env!("CARGO_MANIFEST_DIR").unwrap_or(".");
    Path::new(root).join(FIXTURE)
}

/// Reads the fixture.
fn read_fixture(path: &Path) -> Result<Fixture, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return Err(format!(
                "the fixture is missing: {} ({error}). The fixture is \
                 committed in crates/tjs/tests/fixtures/corpus_digest.txt",
                path.display()
            ));
        }
    };
    let mut fixture = Fixture {
        digests: BTreeMap::new(),
        skipped: BTreeSet::new(),
    };
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((first, second)) = line.split_once(char::is_whitespace) else {
            return Err(format!(
                "{}:{}: the line has no path",
                path.display(),
                number + 1
            ));
        };
        let second = second.trim();
        match first {
            "skip" => {
                if second.is_empty() {
                    return Err(format!(
                        "{}:{}: the skipped script has no path",
                        path.display(),
                        number + 1
                    ));
                }
                fixture.skipped.insert(second.to_string());
            }
            digest => {
                if second.is_empty() || digest.len() != 64 {
                    return Err(format!(
                        "{}:{}: the line is not a digest and a path",
                        path.display(),
                        number + 1
                    ));
                }
                fixture
                    .digests
                    .insert(second.to_string(), digest.to_string());
            }
        }
    }
    Ok(fixture)
}

/// Writes the digests the run produced.
fn write_record(path: &Path, lines: &[String]) -> Result<(), String> {
    let mut text = String::new();
    for line in lines {
        let _ = writeln!(text, "{line}");
    }
    std::fs::write(path, text).map_err(|error| format!("cannot write {} ({error})", path.display()))
}

/// Whether the script is one of the generated scripts.
fn is_generated(relative: &str) -> bool {
    relative.starts_with(&format!("{GENERATED_DIRECTORY}/"))
}

/// The scripts of the corpus, with their path under the corpus root.
fn corpus_scripts(corpus: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut scripts = Vec::new();
    // the directory and its path under the corpus root
    let mut directories = vec![(corpus.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = directories.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                return Err(format!("cannot read {} ({error})", directory.display()));
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    return Err(format!("cannot read {} ({error})", directory.display()));
                }
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().replace('\\', "/");
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if path.is_dir() {
                directories.push((path, relative));
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("tjs") {
                continue;
            }
            scripts.push((relative, path));
        }
    }
    scripts.sort();
    Ok(scripts)
}

/// The canonical form of one script file.
fn canonical_of_file(path: &Path) -> Result<String, String> {
    let units = read_units(path)?;
    canonical_form(&units)
}

/// The code units of a script file.
fn read_units(path: &Path) -> Result<Vec<u16>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => return Err(format!("cannot read {} ({error})", path.display())),
    };
    let mut units = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        index = 2;
    }
    while index + 1 < bytes.len() {
        units.push(u16::from_le_bytes([bytes[index], bytes[index + 1]]));
        index += 2;
    }
    Ok(units)
}

/// The canonical form of one compiled script block.
///
/// The form is a property of the block, not of the way the block is
/// carried: the code stream, the constant area, the counts, the source
/// positions, the relations and the member registrations of every
/// context, and the block's own shape.
fn canonical_form(units: &[u16]) -> Result<String, String> {
    let mut lexer = Lexer::new(units);
    let mut generator = Generator::new();
    let parsed = Parser::new(&mut lexer, &mut generator).parse();
    let contexts = generator.contexts();

    let mut form = String::new();
    let top = contexts
        .iter()
        .position(|context| context.kind == ContextType::TopLevel);
    let _ = writeln!(
        form,
        "block contexts={} top={} errors={}",
        contexts.len(),
        match top {
            Some(index) => index.to_string(),
            None => "-1".to_string(),
        },
        parsed.error_count
    );
    for (index, context) in contexts.iter().enumerate() {
        let _ = writeln!(
            form,
            "context {index} kind={} name={}",
            context_kind(context.kind),
            match context.name.as_ref() {
                Some(name) => hex_text(name),
                None => "none".to_string(),
            }
        );

        let _ = write!(form, "  code {}", context.code().len());
        for word in context.code() {
            let _ = write!(form, " {word}");
        }
        let _ = writeln!(form);

        let _ = write!(form, "  constants {}", context.data().len());
        for value in context.data() {
            let _ = write!(form, " {}", value_text(value)?);
        }
        let _ = writeln!(form);

        let _ = writeln!(
            form,
            "  counts maxvar={} reserve={} maxframe={} args={} argbase={} collapse={}",
            context.max_variable_count,
            context.kind.variable_reserve_count(),
            context.max_frame_count,
            context.func_decl_arg_count,
            context.func_decl_unnamed_arg_array_base,
            context.func_decl_collapse_base
        );

        let _ = write!(form, "  labels {}", context.source_positions().len());
        for label in context.source_positions() {
            let _ = write!(form, " {}:{}", label.code_pos, label.source_pos);
        }
        let _ = writeln!(form);

        let _ = write!(
            form,
            "  relations parent={} setter={} getter={} super={} superptr={}",
            relation(context.parent),
            relation(context.prop_setter),
            relation(context.prop_getter),
            relation(context.super_class_getter),
            context.super_class_getter_pointer.len()
        );
        for place in &context.super_class_getter_pointer {
            let _ = write!(form, " {place}");
        }
        let _ = writeln!(form);

        let _ = write!(form, "  members {}", context.properties().len());
        // the members are sorted by their name: the order the members are
        // registered in is not a property of the object that holds them
        let mut properties: Vec<(&str, usize)> = context
            .properties()
            .iter()
            .map(|property| (property.name.as_str(), property.member))
            .collect();
        properties.sort();
        for (name, member) in properties {
            let _ = write!(form, " {}={}", hex_text(name), member);
        }
        let _ = writeln!(form);
    }
    Ok(form)
}

/// The text of an object with the addresses taken out.
///
/// An object turned into a string carries the object's addresses, and the
/// address is where the heap happened to be: the engine differs from itself
/// the same way. The canonical form therefore carries the text of an object
/// without its addresses, and the digest is a property of the compiled
/// script and not of where the heap happened to be.
fn object_text_without_addresses(text: &str) -> Option<String> {
    let rest = text.strip_prefix("(object ")?;
    let (first, rest) = rest.split_once(':')?;
    let second = rest.strip_suffix(')')?;
    if !is_address(first) || !is_address(second) {
        return None;
    }
    Some("(object 0x0:0x0)".to_string())
}

/// Whether the text is an address: they are printed in the hexadecimal
/// form with the `0x` in front.
fn is_address(text: &str) -> bool {
    let Some(digits) = text.strip_prefix("0x") else {
        return false;
    };
    !digits.is_empty() && digits.chars().all(|digit| digit.is_ascii_hexdigit())
}

/// The number of a context's kind.
fn context_kind(kind: ContextType) -> i32 {
    match kind {
        ContextType::TopLevel => 0,
        ContextType::Function => 1,
        ContextType::ExprFunction => 2,
        ContextType::Property => 3,
        ContextType::PropertySetter => 4,
        ContextType::PropertyGetter => 5,
        ContextType::Class => 6,
        ContextType::SuperClassGetter => 7,
    }
}

/// The index of a relation, or -1 when there is none.
fn relation(index: Option<usize>) -> String {
    match index {
        Some(index) => index.to_string(),
        None => "-1".to_string(),
    }
}

/// The text of one value of the constant area.
fn value_text(value: &Value) -> Result<String, String> {
    match value {
        Value::Void => Ok("void".to_string()),
        Value::Integer(number) => Ok(format!("int:{number}")),
        Value::Real(number) => Ok(format!("real:{:016x}", number.to_bits())),
        Value::String(text) => {
            // the text of an object carries the object's addresses, and
            // the address is where the heap happened to be, so the text is
            // taken out of it: the text of an object is the same for
            // every object
            let text = match object_text_without_addresses(text) {
                Some(text) => text,
                None => text.clone(),
            };
            let units: Vec<u16> = text.encode_utf16().collect();
            Ok(format!("string:{}", hex_units(&units)))
        }
        Value::Octet(bytes) => Ok(format!("octet:{}", hex_bytes(bytes))),
        Value::Object(id) => {
            if id.is_null() {
                Ok("object:null".to_string())
            } else {
                Ok("object:other".to_string())
            }
        }
        Value::Context(index) => Ok(format!("context:{index}")),
        Value::ConstArray { elements, .. } => {
            let mut text = String::from("array:[");
            for (index, element) in elements.iter().enumerate() {
                if index != 0 {
                    text.push(',');
                }
                text.push_str(&value_text(element)?);
            }
            text.push(']');
            Ok(text)
        }
        Value::ConstDictionary { entries, .. } => {
            // the members are sorted by their name: the dictionary holds
            // them in the order of its own hash, and the order is not a
            // property of the value
            let mut members: Vec<(String, String)> = entries
                .iter()
                .map(|(name, value)| {
                    let name = match name.to_text() {
                        Ok(text) => hex_text(&text),
                        Err(refusal) => {
                            return Err(format!(
                                "a dictionary member's name is not a text: {refusal}"
                            ));
                        }
                    };
                    Ok((name, value_text(value)?))
                })
                .collect::<Result<_, String>>()?;
            members.sort();
            let mut text = String::from("dictionary:{");
            for (index, (name, value)) in members.iter().enumerate() {
                if index != 0 {
                    text.push(',');
                }
                text.push_str(name);
                text.push('=');
                text.push_str(value);
            }
            text.push('}');
            Ok(text)
        }
    }
}

/// The hex of the code units of a text.
fn hex_text(text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    hex_units(&units)
}

/// The hex of a run of code units.
fn hex_units(units: &[u16]) -> String {
    let mut text = String::with_capacity(units.len() * 4);
    for unit in units {
        let _ = write!(text, "{unit:04x}");
    }
    text
}

/// The hex of a run of bytes.
fn hex_bytes(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The SHA-256 of the bytes, as the hex of the digest.
fn sha256_hex(bytes: &[u8]) -> String {
    const ROUND: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let mut message = bytes.to_vec();
    let length = (bytes.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&length.to_be_bytes());

    let mut words = [0u32; 64];
    for chunk in message.chunks(64) {
        for index in 0..16 {
            let mut word = [0u8; 4];
            word.copy_from_slice(&chunk[index * 4..index * 4 + 4]);
            words[index] = u32::from_be_bytes(word);
        }
        for index in 16..64 {
            let first = words[index - 15];
            let second = words[index - 2];
            let sum0 = first.rotate_right(7) ^ first.rotate_right(18) ^ (first >> 3);
            let sum1 = second.rotate_right(17) ^ second.rotate_right(19) ^ (second >> 10);
            words[index] = words[index - 16]
                .wrapping_add(sum0)
                .wrapping_add(words[index - 7])
                .wrapping_add(sum1);
        }

        let mut a = state[0];
        let mut b = state[1];
        let mut c = state[2];
        let mut d = state[3];
        let mut e = state[4];
        let mut f = state[5];
        let mut g = state[6];
        let mut h = state[7];
        for index in 0..64 {
            let sum = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let first = h
                .wrapping_add(sum)
                .wrapping_add(choose)
                .wrapping_add(ROUND[index])
                .wrapping_add(words[index]);
            let sum = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let second = sum.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(first);
            d = c;
            c = b;
            b = a;
            a = first.wrapping_add(second);
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
        state[5] = state[5].wrapping_add(f);
        state[6] = state[6].wrapping_add(g);
        state[7] = state[7].wrapping_add(h);
    }

    let mut digest = String::with_capacity(64);
    for word in state {
        let _ = write!(digest, "{word:08x}");
    }
    digest
}
