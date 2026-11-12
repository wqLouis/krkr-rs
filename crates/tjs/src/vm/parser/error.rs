//! The failures the parser reports.

/// A failure the parser reports.
///
/// A failure carries the place in the script where the parser stopped,
/// counted in code units from the start of the script, and the message
/// that names what the parser found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// The token stream does not fit the grammar.
    Syntax {
        /// The message the script block raises.
        message: String,
        /// The place in the script where the parser stopped.
        position: i32,
        /// The token the parser expected, when it knows one. The message
        /// does not name it.
        expected: Option<String>,
    },
    /// The analyser stopped: the script is not a stream of tokens.
    Lexical {
        /// The message that names the failure.
        message: String,
        /// The place in the script where the analyser stopped.
        position: i32,
    },
    /// Too many failures: the parser stopped.
    Aborted,
    /// The generator refused the tree.
    Generator {
        /// The message that names the failure.
        message: String,
    },
}

impl ParseError {
    /// The place in the script where the parser stopped.
    pub fn position(&self) -> i32 {
        match self {
            Self::Syntax { position, .. } | Self::Lexical { position, .. } => *position,
            Self::Aborted => -1,
            Self::Generator { .. } => -1,
        }
    }

    /// The message that names the failure.
    pub fn message(&self) -> &str {
        match self {
            Self::Syntax { message, .. } | Self::Lexical { message, .. } => message,
            Self::Aborted => "too many syntax errors",
            Self::Generator { message } => message,
        }
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax {
                message, position, ..
            } => {
                write!(formatter, "{message} at {position}")
            }
            Self::Lexical { message, position } => {
                write!(formatter, "{message} at {position}")
            }
            Self::Aborted => write!(formatter, "too many syntax errors"),
            Self::Generator { message } => write!(formatter, "{message}"),
        }
    }
}

impl From<crate::vm::codegen::CodegenError> for ParseError {
    fn from(error: crate::vm::codegen::CodegenError) -> Self {
        Self::Generator {
            message: error.to_string(),
        }
    }
}

impl std::error::Error for ParseError {}
