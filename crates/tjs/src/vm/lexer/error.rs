//! The failures the lexical analyser reports.

/// A lexical failure.
///
/// Every failure carries the position in the script where the analyser
/// stopped, counted in code units from the start of the script.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LexError {
    /// A string, a regular expression or a binary literal was not
    /// terminated.
    #[error("Un-terminated string/regexp/octet literal")]
    UnterminatedLiteral {
        /// Where the analyser stopped.
        position: usize,
    },

    /// A number could not be parsed.
    #[error("Cannot be parsed as a number")]
    Number {
        /// Where the analyser stopped.
        position: usize,
    },

    /// A comment was not terminated.
    #[error("Un-terminated comment")]
    UnterminatedComment {
        /// Where the analyser stopped.
        position: usize,
    },

    /// A character that cannot begin a token.
    #[error("Invalid character '{character}'")]
    InvalidCharacter {
        /// Where the character is.
        position: usize,
        /// The character, written the way a message writes it.
        character: String,
    },

    /// A conditional-compilation statement or expression is malformed.
    #[error("Error in conditional compiling expression")]
    ConditionalCompilation {
        /// Where the analyser stopped.
        position: usize,
    },
}

impl LexError {
    /// The position in the script where the analyser stopped.
    pub fn position(&self) -> usize {
        match self {
            Self::UnterminatedLiteral { position }
            | Self::Number { position }
            | Self::UnterminatedComment { position }
            | Self::InvalidCharacter { position, .. }
            | Self::ConditionalCompilation { position } => *position,
        }
    }
}
