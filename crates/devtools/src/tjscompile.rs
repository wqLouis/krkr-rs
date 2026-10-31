//! The TJS command family: compile a script into a bytecode file.
//!
//! The family is built only with the `tjs-compile` feature, because it links
//! the vendored VM.

use std::path::{Path, PathBuf};

use crate::cli::{self, CommandSpec, OptionSpec};
use crate::error::ToolError;

/// The family's usage text.
const USAGE: &str = "\
TJS compiler:

  krkr-devtools tjs compile <in.tjs> -o <out.tjsb>

The script is compiled as a statement script. The output path is created when
its directory does not exist.
";

/// The `compile` command's contract.
const COMPILE: CommandSpec = CommandSpec {
    min_positionals: 1,
    max_positionals: 1,
    options: &[OptionSpec {
        key: "output",
        spellings: &["-o", "--output"],
    }],
};

/// Run the family.
pub fn run(argv: &[String]) -> Result<(), ToolError> {
    let Some(command) = argv.first() else {
        crate::out::write_line(&mut std::io::stderr().lock(), "<stderr>", USAGE)?;
        return Err(ToolError::Usage("a command is required".to_string()));
    };
    let rest = &argv[1..];
    match command.as_str() {
        "compile" => compile(rest),
        "--help" | "-h" | "help" => {
            let mut out = crate::out::Out::standard();
            out.text(USAGE)?;
            Ok(())
        }
        other => Err(ToolError::Usage(format!("unknown tjs command `{other}`"))),
    }
}

/// Compile a script into a bytecode file.
fn compile(argv: &[String]) -> Result<(), ToolError> {
    let invocation = cli::parse(argv, &COMPILE)?;
    let input = cli::positional(&invocation, 0)?;
    let output = invocation.value("output").ok_or_else(|| {
        ToolError::Usage("`compile` needs the output path (`-o <path>`)".to_string())
    })?;
    let script = std::fs::read_to_string(input).map_err(|source| ToolError::Host {
        path: PathBuf::from(input),
        source,
    })?;
    let target = std::path::absolute(output).map_err(|source| ToolError::Host {
        path: PathBuf::from(output),
        source,
    })?;
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| ToolError::Host {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let engine =
        tjs::Tjs2Engine::new().map_err(|message| ToolError::Compile(message.to_string()))?;
    let name = Path::new(input)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| input.to_string());
    engine
        .compile_script(
            &script,
            &target.to_string_lossy(),
            tjs::CompileOptions {
                isresult: false,
                outputdebug: false,
                isexpression: false,
                name: &name,
                lineofs: 0,
            },
        )
        .map_err(|error| ToolError::Compile(error.to_string()))?;
    let mut out = crate::out::Out::standard();
    out.line(&format!("wrote {output}"))?;
    Ok(())
}
