//! The literal forms: strings, regular expressions and binary buffers.

use super::{Comment, Cursor, LexError};

/// The text a literal scan builds.
///
/// It is the scanner's own buffer: the analyser copies the text into its
/// own string buffer once the literal is complete.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Text {
    units: Vec<u16>,
}

impl Text {
    /// An empty text.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one code unit.
    pub fn push(&mut self, unit: u16) {
        self.units.push(unit);
    }

    /// The code units of the text.
    pub fn units(&self) -> &[u16] {
        &self.units
    }

    /// Whether the text holds no code unit.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }
}

/// How a string literal ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StringEnd {
    /// The closing delimiter.
    Delimiter,
    /// An embedded expression written `&expression;`.
    Ampersand,
    /// An embedded expression written `${expression}`.
    Dollar,
}

/// What a string scan produced.
pub(crate) struct StringScan {
    /// The text of the literal, with the escapes decoded.
    pub text: Text,
    /// How the literal ended.
    pub end: StringEnd,
}

impl Cursor {
    /// The string literal that starts at the current position.
    ///
    /// The cursor is at the opening delimiter; it is left after the literal.
    ///
    /// # Errors
    ///
    /// A literal that is not terminated is a [`LexError`].
    pub fn scan_string(
        &mut self,
        delimiter: u16,
        embeddable: bool,
    ) -> Result<StringScan, LexError> {
        if self.current().is_none() {
            return Err(self.unterminated_literal());
        }
        self.advance();
        self.scan_string_content(delimiter, embeddable)
    }

    /// The content of a string literal, with the escapes decoded.
    ///
    /// The cursor is at the first code unit after the opening delimiter.
    ///
    /// # Errors
    ///
    /// A literal that is not terminated is a [`LexError`].
    pub fn scan_string_content(
        &mut self,
        delimiter: u16,
        embeddable: bool,
    ) -> Result<StringScan, LexError> {
        let mut text = Text::new();
        loop {
            let Some(character) = self.current() else {
                return Err(self.unterminated_literal());
            };
            if character == u16::from(b'\\') {
                // an escape
                if !self.advance() {
                    return Err(self.unterminated_literal());
                }
                let Some(escaped) = self.current() else {
                    return Err(self.unterminated_literal());
                };
                if escaped == u16::from(b'x') || escaped == u16::from(b'X') {
                    // a hexadecimal escape, at most four digits
                    if !self.advance() {
                        return Err(self.unterminated_literal());
                    }
                    let mut code: u32 = 0;
                    let mut digits = 0;
                    while digits < 4 {
                        let Some(digit) = self.current().and_then(hex_digit) else {
                            break;
                        };
                        code = code * 16 + u32::from(digit);
                        digits += 1;
                        if !self.advance() {
                            break;
                        }
                    }
                    if self.current().is_none() {
                        return Err(self.unterminated_literal());
                    }
                    // the escape is one code unit wide, so a value that does
                    // not fit keeps its low half
                    text.push(code as u16);
                } else if escaped == u16::from(b'0') {
                    // an octal escape
                    if !self.advance() {
                        return Err(self.unterminated_literal());
                    }
                    let mut code: u32 = 0;
                    while let Some(digit) = self.current().and_then(octal_digit) {
                        code = code * 8 + u32::from(digit);
                        if !self.advance() {
                            break;
                        }
                    }
                    if self.current().is_none() {
                        return Err(self.unterminated_literal());
                    }
                    text.push(code as u16);
                } else {
                    text.push(unescape(escaped));
                    self.advance();
                }
            } else if character == delimiter {
                // the closing delimiter, unless another literal follows
                if !self.advance() {
                    return Ok(StringScan {
                        text,
                        end: StringEnd::Delimiter,
                    });
                }
                let next = self.skip_space_at(self.position());
                if self.unit_at(next) == Some(delimiter) {
                    self.move_to(next);
                    self.advance();
                } else {
                    return Ok(StringScan {
                        text,
                        end: StringEnd::Delimiter,
                    });
                }
            } else if embeddable && character == u16::from(b'&') {
                if !self.advance() {
                    return Err(self.unterminated_literal());
                }
                return Ok(StringScan {
                    text,
                    end: StringEnd::Ampersand,
                });
            } else if embeddable && character == u16::from(b'$') {
                // a dollar sign starts an embedded expression only when a
                // brace follows it
                let start = self.position();
                if !self.advance() {
                    return Err(self.unterminated_literal());
                }
                if self.current() == Some(u16::from(b'{')) {
                    if !self.advance() {
                        return Err(self.unterminated_literal());
                    }
                    return Ok(StringScan {
                        text,
                        end: StringEnd::Dollar,
                    });
                }
                self.move_to(start);
                text.push(character);
                self.advance();
            } else {
                text.push(character);
                self.advance();
            }
        }
    }

    /// The regular expression literal that starts at the current position.
    ///
    /// The cursor is at the opening slash; it is left after the literal.
    /// The text is the internal form the regular expression compiler reads:
    /// the flags, then the pattern, between two slashes.
    ///
    /// # Errors
    ///
    /// A literal that is not terminated is a [`LexError`].
    pub fn scan_regexp(&mut self) -> Result<Text, LexError> {
        let mut pattern = Text::new();
        let mut escaped = false;
        loop {
            let Some(character) = self.current() else {
                return Err(self.unterminated_literal());
            };
            if character == u16::from(b'\\') {
                pattern.push(character);
                escaped = !escaped;
            } else if character == u16::from(b'/') && !escaped {
                // the closing slash, then the flags
                if !self.advance() {
                    // the text ended at the closing slash: the value is the
                    // pattern
                    return Ok(pattern);
                }
                let mut flags = Text::new();
                while let Some(flag) = self.current() {
                    if flag < u16::from(b'a') || flag > u16::from(b'z') {
                        break;
                    }
                    flags.push(flag);
                    if !self.advance() {
                        break;
                    }
                }
                let mut literal = Text::new();
                literal.push(u16::from(b'/'));
                literal.push(u16::from(b'/'));
                for unit in flags.units() {
                    literal.push(*unit);
                }
                literal.push(u16::from(b'/'));
                for unit in pattern.units() {
                    literal.push(*unit);
                }
                return Ok(literal);
            } else {
                escaped = false;
                pattern.push(character);
            }
            self.advance();
        }
    }

    /// The binary literal that starts at the current position.
    ///
    /// The cursor is at the opening angle bracket; it is left after the
    /// literal. The literal holds the bytes between the angle bracket and
    /// the percent sign, written as hexadecimal digits.
    ///
    /// # Errors
    ///
    /// A literal that is not terminated is a [`LexError`].
    pub fn scan_octet(&mut self) -> Result<Vec<u8>, LexError> {
        self.advance();
        self.advance();
        let mut bytes: Vec<u8> = Vec::new();
        let mut leading = true;
        let mut current: u8 = 0;
        loop {
            let Some(character) = self.current() else {
                return Err(self.unterminated_literal());
            };
            match self.skip_comment() {
                Comment::Skipped => continue,
                Comment::Ended => return Err(self.unterminated_literal()),
                Comment::Unterminated => {
                    return Err(LexError::UnterminatedComment {
                        position: self.position(),
                    });
                }
                Comment::Absent => {}
            }
            let next = self.position() + 1;
            if character == u16::from(b'%') && self.unit_at(next) == Some(u16::from(b'>')) {
                // the literal ends
                self.move_to(next);
                self.advance();
                if !leading {
                    bytes.push(current);
                }
                return Ok(bytes);
            }
            if let Some(digit) = hex_digit(character) {
                if leading {
                    current = digit;
                    leading = false;
                } else {
                    current = (current << 4).wrapping_add(digit);
                    bytes.push(current);
                    leading = true;
                }
            }
            if !leading && character == u16::from(b',') {
                bytes.push(current);
                leading = true;
            }
            self.move_to(next);
        }
    }

    /// The error for a literal that is not terminated.
    fn unterminated_literal(&self) -> LexError {
        LexError::UnterminatedLiteral {
            position: self.position(),
        }
    }
}

/// The value of a hexadecimal digit.
fn hex_digit(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x39 => Some((unit - 0x30) as u8),
        0x41..=0x46 => Some((unit - 0x41 + 10) as u8),
        0x61..=0x66 => Some((unit - 0x61 + 10) as u8),
        _ => None,
    }
}

/// The value of an octal digit.
fn octal_digit(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x37 => Some((unit - 0x30) as u8),
        _ => None,
    }
}

/// The character an escape stands for.
fn unescape(unit: u16) -> u16 {
    match unit {
        0x61 => 0x07,
        0x62 => 0x08,
        0x66 => 0x00c,
        0x6e => 0x0a,
        0x72 => 0x0d,
        0x74 => 0x09,
        0x76 => 0x0b,
        other => other,
    }
}
