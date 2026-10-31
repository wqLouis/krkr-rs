//! `krkr-devtools` — the krkr-rs developer tools.
//!
//! The binary hosts three command families:
//!
//! * `xp3` — list, inspect and extract the entries of an XP3 archive,
//!   through the L1 container parser.
//! * `tjsb` — inspect and disassemble a TJS2 bytecode file.
//! * `tjs` — compile a TJS script into a bytecode file. The family is built
//!   only with the `tjs-compile` feature, because it links the vendored VM.
//!
//! The tool is a host program for the development host: it is not part of
//! the engine's runtime architecture, and it links no engine layer.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(not(target_os = "linux"))]
compile_error!("krkr-devtools is a Linux-only development tool");

mod cli;
mod error;
mod out;

#[cfg(feature = "tjsb")]
mod tjsb;
#[cfg(feature = "tjs-compile")]
mod tjscompile;
#[cfg(feature = "xp3")]
mod xp3;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match cli::run(&argv) {
        Ok(()) => {}
        // The reader closed the pipe (`| head`): the command has nothing
        // left to say, and its end is clean.
        Err(error::ToolError::PipeClosed) => {}
        Err(error) => {
            let message = format!("krkr-devtools: {error}");
            // The error output may be gone too; the status still says what
            // happened.
            drop(crate::out::write_line(
                &mut std::io::stderr().lock(),
                "<stderr>",
                &message,
            ));
            std::process::exit(error.exit_code());
        }
    }
}
