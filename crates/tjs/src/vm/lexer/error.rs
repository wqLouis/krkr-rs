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

/// Write code units the way a message writes them: a character as itself,
/// the control characters and the quote and backslash as escapes.
///
/// A hexadecimal digit that follows a control character is written as an
/// escape as well.
///
/// The text ends at the first NUL.
///
/// A code unit that cannot be written as a character — a lone surrogate —
/// is written as the replacement character, because a message cannot hold
/// it.
pub fn escape_characters(units: &[u16]) -> String {
    let mut text = String::new();
    // Whether the last character written was a control character.
    let mut hexadecimal = false;
    for &unit in units {
        if unit == 0 {
            break;
        }
        let escape = match unit {
            0x07 => Some("\\a"),
            0x08 => Some("\\b"),
            0x0c => Some("\\f"),
            0x0a => Some("\\n"),
            0x0d => Some("\\r"),
            0x09 => Some("\\t"),
            0x0b => Some("\\v"),
            0x5c => Some("\\\\"),
            0x27 => Some("\\'"),
            0x22 => Some("\\\""),
            _ => None,
        };
        if let Some(escape) = escape {
            text.push_str(escape);
            hexadecimal = false;
            continue;
        }
        if hexadecimal && is_hexadecimal(unit) {
            push_hexadecimal(&mut text, unit);
            continue;
        }
        if unit < 0x20 {
            hexadecimal = true;
            push_hexadecimal(&mut text, unit);
            continue;
        }
        hexadecimal = false;
        match char::from_u32(u32::from(unit)) {
            Some(character) => text.push(character),
            None => text.push('\u{fffd}'),
        }
    }
    text
}

/// Whether a code unit is a hexadecimal digit.
fn is_hexadecimal(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
}

/// Write a code unit as a hexadecimal escape.
fn push_hexadecimal(text: &mut String, unit: u16) {
    text.push_str(&format!("\\x{unit:02x}"));
}

#[cfg(test)]
mod tests {
    use super::escape_characters;

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn a_control_character_is_written_as_a_hexadecimal_escape() {
        assert_eq!(escape_characters(&[0x01]), "\\x01");
        assert_eq!(escape_characters(&[0x1b]), "\\x1b");
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn a_hexadecimal_digit_after_a_control_character_is_written_as_an_escape() {
        assert_eq!(escape_characters(&[0x01, 0x61]), "\\x01\\x61");
        assert_eq!(escape_characters(&[0x01, 0x41]), "\\x01\\x41");
        assert_eq!(escape_characters(&[0x01, 0x30]), "\\x01\\x30");
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn every_hexadecimal_digit_after_a_control_character_is_written_as_an_escape() {
        assert_eq!(
            escape_characters(&[0x01, 0x61, 0x62, 0x63]),
            "\\x01\\x61\\x62\\x63"
        );
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn a_character_that_is_not_a_hexadecimal_digit_ends_the_escapes() {
        assert_eq!(escape_characters(&[0x01, 0x7a]), "\\x01z");
        assert_eq!(escape_characters(&[0x01, 0x2f]), "\\x01/");
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn a_named_escape_ends_the_hexadecimal_escapes() {
        assert_eq!(escape_characters(&[0x01, 0x07, 0x61]), "\\x01\\aa");
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn a_hexadecimal_digit_on_its_own_is_written_as_itself() {
        assert_eq!(escape_characters(&[0x61]), "a");
        assert_eq!(escape_characters(&[0x30]), "0");
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn the_control_characters_have_their_own_escapes() {
        assert_eq!(escape_characters(&[0x07]), "\\a");
        assert_eq!(escape_characters(&[0x08]), "\\b");
        assert_eq!(escape_characters(&[0x09]), "\\t");
        assert_eq!(escape_characters(&[0x0a]), "\\n");
        assert_eq!(escape_characters(&[0x0b]), "\\v");
        assert_eq!(escape_characters(&[0x0c]), "\\f");
        assert_eq!(escape_characters(&[0x0d]), "\\r");
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn the_quote_and_the_backslash_are_escaped() {
        assert_eq!(escape_characters(&[0x27]), "\\'");
        assert_eq!(escape_characters(&[0x22]), "\\\"");
        assert_eq!(escape_characters(&[0x5c]), "\\\\");
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160
    #[test]
    fn the_text_ends_at_the_first_nul() {
        assert_eq!(escape_characters(&[0x61, 0x00, 0x62]), "a");
        assert_eq!(escape_characters(&[0x00]), "");
    }
}
