//! The TJS bytecode command family: inspect and disassemble a bytecode file.
//!
//! The tool is a disassembler, not a decompiler: it shows the objects, their
//! opcodes, their operands and the constants they name. It never rebuilds
//! source text from the bytecode.

mod disasm;
pub mod format;
pub mod opcode;

pub use format::BytecodeError;

use std::path::Path;

use crate::cli::{self, CommandSpec, OptionSpec};
use crate::error::ToolError;

/// The family's usage text.
const USAGE: &str = "\
TJS bytecode tools:

  krkr-devtools tjsb info <file.tjsb>
  krkr-devtools tjsb strings <file.tjsb> [--min-len N]
  krkr-devtools tjsb disasm <file.tjsb> [--object N] [--grep TEXT]

`info` prints the file's shape and one line per object. `strings` prints the
file's string table, keeping the strings of at least N characters. `disasm`
prints the listing: `--object N` limits it to one object and `--grep TEXT`
keeps only the lines that hold TEXT.
";

/// The `info` command's contract.
const INFO: CommandSpec = CommandSpec {
    min_positionals: 1,
    max_positionals: 1,
    options: &[],
};

/// The `strings` command's contract.
const STRINGS: CommandSpec = CommandSpec {
    min_positionals: 1,
    max_positionals: 1,
    options: &[OptionSpec {
        key: "min-len",
        spellings: &["--min-len"],
        takes_value: true,
    }],
};

/// The `disasm` command's contract.
const DISASM: CommandSpec = CommandSpec {
    min_positionals: 1,
    max_positionals: 1,
    options: &[
        OptionSpec {
            key: "object",
            spellings: &["--object"],
            takes_value: true,
        },
        OptionSpec {
            key: "grep",
            spellings: &["--grep"],
            takes_value: true,
        },
    ],
};

/// Run the family.
pub fn run(argv: &[String]) -> Result<(), ToolError> {
    let Some(command) = argv.first() else {
        eprintln!("{USAGE}");
        return Err(ToolError::Usage("a command is required".to_string()));
    };
    let rest = &argv[1..];
    match command.as_str() {
        "info" => info(rest),
        "strings" => strings(rest),
        "disasm" => listing(rest),
        "--help" | "-h" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        other => Err(ToolError::Usage(format!("unknown tjsb command `{other}`"))),
    }
}

/// Print the file's shape and its objects.
fn info(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &INFO)?;
    let path = cli::positional(&invocation, 0)?;
    let bytes = read_file(Path::new(path))?;
    let file = format::parse(&bytes).map_err(ToolError::Bytecode)?;
    println!("file: {path}");
    println!("size: {} bytes", bytes.len());
    println!("toplevel: {}", file.toplevel);
    println!("objects: {}", file.objects.len());
    println!(
        "constants: bytes={} shorts={} longs={} longlongs={} doubles={} strings={} octets={}",
        file.constants.bytes.len(),
        file.constants.shorts.len(),
        file.constants.longs.len(),
        file.constants.longlongs.len(),
        file.constants.doubles.len(),
        file.constants.strings.len(),
        file.constants.octets.len()
    );
    for (index, object) in file.objects.iter().enumerate() {
        println!(
            "object {index}: {}",
            describe_object(object, &file.constants)
        );
    }
    Ok(())
}

/// Print the file's string table.
fn strings(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &STRINGS)?;
    let path = cli::positional(&invocation, 0)?;
    let minimum: usize = match invocation.value("min-len") {
        None => 0,
        Some(text) => text
            .parse()
            .map_err(|_| ToolError::Usage(format!("`{text}` is not a length")))?,
    };
    let bytes = read_file(Path::new(path))?;
    let file = format::parse(&bytes).map_err(ToolError::Bytecode)?;
    for (index, text) in file.constants.strings.iter().enumerate() {
        if text.chars().count() < minimum {
            continue;
        }
        let escaped = disasm::escape(text);
        println!("[{index}] \"{escaped}\"");
    }
    Ok(())
}

/// Print the file's listing.
fn listing(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &DISASM)?;
    let path = cli::positional(&invocation, 0)?;
    let selected: Option<usize> = match invocation.value("object") {
        None => None,
        Some(text) => Some(
            text.parse()
                .map_err(|_| ToolError::Usage(format!("`{text}` is not an object index")))?,
        ),
    };
    let needle = invocation.value("grep");
    let bytes = read_file(Path::new(path))?;
    let file = format::parse(&bytes).map_err(ToolError::Bytecode)?;
    if let Some(selected) = selected
        && selected >= file.objects.len()
    {
        return Err(ToolError::Usage(format!(
            "the file has no object {selected}"
        )));
    }
    for (index, object) in file.objects.iter().enumerate() {
        if let Some(selected) = selected
            && selected != index
        {
            continue;
        }
        let header = describe_object(object, &file.constants);
        if needle.is_none_or(|needle| header.contains(needle)) {
            println!("object {index}: {header}");
        }
        let instructions =
            disasm::disassemble(object, &file.constants).map_err(ToolError::Bytecode)?;
        for instruction in instructions {
            let line = format!(
                "  {:04}  {:<12} {}",
                instruction.address, instruction.name, instruction.operands
            );
            let line = line.trim_end();
            if needle.is_none_or(|needle| line.contains(needle)) {
                println!("{line}");
            }
        }
    }
    Ok(())
}

/// The name of a context kind.
fn context_name(kind: i32) -> &'static str {
    match kind {
        0 => "ctTopLevel",
        1 => "ctFunction",
        2 => "ctExprFunction",
        3 => "ctProperty",
        4 => "ctPropertySetter",
        5 => "ctPropertyGetter",
        6 => "ctClass",
        7 => "ctSuperClassGetter",
        _ => "ctUnknown",
    }
}

/// The name of an object.
fn object_name(object: &format::Object, constants: &format::Constants) -> String {
    match usize::try_from(object.name)
        .ok()
        .and_then(|index| constants.strings.get(index))
    {
        Some(text) => format!("\"{text}\""),
        None => format!("<string {}>", object.name),
    }
}

/// The one-line description of an object.
fn describe_object(object: &format::Object, constants: &format::Constants) -> String {
    let name = object_name(object, constants);
    format!(
        "name={name} ctx={} parent={} maxvar={} varreserve={} maxframe={} args={} code={} vars={} props={}",
        context_name(object.context_type),
        object.parent,
        object.max_variable_count,
        object.variable_reserve_count,
        object.max_frame_count,
        object.argument_count,
        object.code.len(),
        object.data.len(),
        object.properties.len()
    )
}

/// Read a file's bytes.
fn read_file(path: &Path) -> Result<Vec<u8>, ToolError> {
    std::fs::read(path).map_err(|source| ToolError::Host {
        path: path.to_path_buf(),
        source,
    })
}
