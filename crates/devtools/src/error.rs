//! The failures the tool reports and exits with.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// A failure the tool reports.
///
/// Every failure is a value: the tool prints the message and exits with a
/// non-zero status, and it never continues with a partial result.
#[derive(Debug)]
pub enum ToolError {
    /// The command line does not match the command's contract.
    Usage(String),
    /// A host file operation failed.
    Host {
        /// The path the operation was working on.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
    /// The file-system service refused an operation.
    #[cfg(feature = "xp3")]
    Service {
        /// The path the operation was working on.
        path: PathBuf,
        /// The underlying failure.
        source: api::error::FsError,
    },
    /// The archive container is malformed, or the entry is missing.
    #[cfg(feature = "xp3")]
    Archive(fs::xp3::Xp3Error),
    /// The bytecode file is malformed.
    #[cfg(feature = "tjsb")]
    Bytecode(crate::tjsb::BytecodeError),
    /// The script compiler refused the input.
    #[cfg(feature = "tjs-compile")]
    Compile(String),
}

impl ToolError {
    /// The process exit status the failure maps to.
    ///
    /// A usage failure exits with `2`; every other failure exits with `1`.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => formatter.write_str(message),
            Self::Host { path, source } => {
                let path = path.display();
                write!(formatter, "{path}: {source}")
            }
            #[cfg(feature = "xp3")]
            Self::Service { path, source } => {
                let path = path.display();
                write!(formatter, "{path}: {source}")
            }
            #[cfg(feature = "xp3")]
            Self::Archive(source) => write!(formatter, "{source}"),
            #[cfg(feature = "tjsb")]
            Self::Bytecode(source) => write!(formatter, "{source}"),
            #[cfg(feature = "tjs-compile")]
            Self::Compile(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Usage(_) => None,
            Self::Host { source, .. } => Some(source),
            #[cfg(feature = "xp3")]
            Self::Service { source, .. } => Some(source),
            #[cfg(feature = "xp3")]
            Self::Archive(source) => Some(source),
            #[cfg(feature = "tjsb")]
            Self::Bytecode(source) => Some(source),
            #[cfg(feature = "tjs-compile")]
            Self::Compile(_) => None,
        }
    }
}
