//! The tool's output streams.
//!
//! Every line the tool prints goes through these writers. The reader may
//! close the pipe at any moment (`| head`), and that is a clean end of the
//! command, not a failure: the tool stops writing and exits with status 0.

use std::io::{self, Write};
use std::path::PathBuf;

use crate::error::ToolError;

/// The tool's standard output.
pub struct Out {
    handle: io::StdoutLock<'static>,
}

impl Out {
    /// The standard output.
    pub fn standard() -> Out {
        Out {
            handle: io::stdout().lock(),
        }
    }

    /// Write one line.
    pub fn line(&mut self, text: &str) -> Result<(), ToolError> {
        write_line(&mut self.handle, "<stdout>", text)
    }

    /// Write text without a line break.
    pub fn text(&mut self, text: &str) -> Result<(), ToolError> {
        write_text(&mut self.handle, "<stdout>", text)
    }
}

/// Write one line to `writer`.
///
/// `stream` names the output in a failure message. A closed pipe ends the
/// command cleanly; every other failure is an error.
pub fn write_line(writer: &mut impl Write, stream: &str, text: &str) -> Result<(), ToolError> {
    write_bytes(writer, stream, text.as_bytes())?;
    write_bytes(writer, stream, b"\n")
}

/// Write text to `writer` without a line break.
///
/// `stream` names the output in a failure message. A closed pipe ends the
/// command cleanly; every other failure is an error.
pub fn write_text(writer: &mut impl Write, stream: &str, text: &str) -> Result<(), ToolError> {
    write_bytes(writer, stream, text.as_bytes())
}

/// Write bytes to `writer`.
///
/// A closed pipe ends the command cleanly; every other failure is an error.
pub fn write_bytes(writer: &mut impl Write, stream: &str, bytes: &[u8]) -> Result<(), ToolError> {
    match writer.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(ToolError::PipeClosed),
        Err(error) => Err(ToolError::Host {
            path: PathBuf::from(stream),
            source: error,
        }),
    }
}

/// Flush `writer`.
///
/// A closed pipe ends the command cleanly; every other failure is an error.
pub fn flush(writer: &mut impl Write, stream: &str) -> Result<(), ToolError> {
    match writer.flush() {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(ToolError::PipeClosed),
        Err(error) => Err(ToolError::Host {
            path: PathBuf::from(stream),
            source: error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer whose pipe is closed.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A writer that fails for another reason.
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("the disk is full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A closed pipe ends the command cleanly: the tool stops writing and
    /// exits with status 0.
    #[test]
    fn a_closed_pipe_ends_the_command_cleanly() {
        let mut writer = Closed;
        let error = write_line(&mut writer, "<stdout>", "text").expect_err("the pipe is closed");
        assert!(matches!(&error, ToolError::PipeClosed), "{error:?}");
        assert_eq!(error.exit_code(), 0);
    }

    /// Every other write failure stays an error.
    #[test]
    fn another_write_failure_is_an_error() {
        let mut writer = Broken;
        let error = write_line(&mut writer, "<stdout>", "text").expect_err("the write fails");
        assert!(matches!(&error, ToolError::Host { .. }), "{error:?}");
        assert_eq!(error.exit_code(), 1);
    }
}
