//! The number literals.
//!
//! A number is scanned in two steps: the text of the literal is collected,
//! and the collected text is converted to a value. The two steps are kept
//! apart because the same text is read twice — once to find where the
//! literal ends, and once to compute its value.
//!
//! Finding the end is the analyser's own work: which code units continue a
//! literal, and where the next token starts. The value is not: the runtime
//! reads a number out of text, and a literal and a string a script converts
//! are read by that one rule.

use crate::vm::runtime::number::{self, Number};

use super::{Cursor, Digits, Value};

impl Cursor {
    /// The number literal at the current position.
    ///
    /// `None` means the text at the cursor is not a number. The cursor is
    /// only ever at a digit here, or at the point of a fraction: a sign is
    /// an operator and a word is a name, so neither begins a literal.
    pub(super) fn scan_number(&mut self) -> Option<Value> {
        let start = self.position();
        if self.current() == Some(u16::from(b'0')) {
            if !self.advance() {
                return Some(Value::Integer(0));
            }
            match self.current() {
                Some(character) if character == u16::from(b'X') || character == u16::from(b'x') => {
                    if !self.advance() {
                        return None;
                    }
                    return self.scan_non_decimal(start, Digits::Hexadecimal);
                }
                Some(character) if character == u16::from(b'B') || character == u16::from(b'b') => {
                    if !self.advance() {
                        return None;
                    }
                    return self.scan_non_decimal(start, Digits::Binary);
                }
                Some(character) if character == u16::from(b'.') => {
                    self.move_to(start);
                }
                Some(character) if character == u16::from(b'E') || character == u16::from(b'e') => {
                    self.move_to(start);
                }
                Some(character) if character == u16::from(b'P') || character == u16::from(b'p') => {
                    return None;
                }
                _ => {
                    self.move_to(start);
                    return self.scan_non_decimal(start, Digits::Octal);
                }
            }
        }

        self.scan_decimal(start)
    }

    /// A decimal number literal at the current position.
    ///
    /// `start` is where the literal begins.
    fn scan_decimal(&mut self, start: usize) -> Option<Value> {
        if !self.skip_number(Digits::Decimal, *b"Ee") {
            return None;
        }
        self.literal_value(start)
    }

    /// A hexadecimal, octal or binary number literal at the current
    /// position.
    ///
    /// `start` is where the literal begins, the `0` of its prefix included.
    fn scan_non_decimal(&mut self, start: usize, digits: Digits) -> Option<Value> {
        if !self.skip_number(digits, *b"Pp") {
            return None;
        }
        self.literal_value(start)
    }

    /// The value of the number literal whose text runs from `start` to the
    /// current position.
    ///
    /// The literal's text is one whole number, so the runtime has to read
    /// all of it. A text it stops short of means the analyser and the
    /// runtime disagree about where a literal ends, and the literal is
    /// refused instead of being read only in part.
    fn literal_value(&self, start: usize) -> Option<Value> {
        let text = &self.units()[start..self.position()];
        let parsed = number::parse(text).ok()?;
        if parsed.consumed != text.len() {
            return None;
        }
        Some(match parsed.number {
            Number::Integer(value) => Value::Integer(value),
            Number::Real(value) => Value::Real(value),
        })
    }

    /// Steps the cursor past the text of a number literal, and answers
    /// whether there was a text.
    ///
    /// The text is the digits of the given base before an exponent mark, at
    /// most one point, and, after the mark, the digits that follow it. The
    /// current position is left after the literal. What the text is worth is
    /// decided by the runtime, not here: the analyser says where a literal
    /// ends, and the runtime reads the text the analyser covers.
    fn skip_number(&mut self, digits: Digits, exponent_marks: [u8; 2]) -> bool {
        let mut taken = false;
        let mut point_found = false;
        let mut exponent_found = false;
        while let Some(character) = self.current() {
            if digits.value(character).is_some() && !exponent_found {
                taken = true;
                if !self.advance() {
                    break;
                }
            } else if character == u16::from(b'.') && !point_found && !exponent_found {
                point_found = true;
                taken = true;
                if !self.advance() {
                    break;
                }
            } else if (character == u16::from(exponent_marks[0])
                || character == u16::from(exponent_marks[1]))
                && !exponent_found
            {
                exponent_found = true;
                taken = true;
                if !self.advance() {
                    break;
                }
                if !self.skip_space() {
                    break;
                }
                if self
                    .current()
                    .is_some_and(|sign| sign == u16::from(b'+') || sign == u16::from(b'-'))
                {
                    if !self.advance() {
                        break;
                    }
                    if !self.skip_space() {
                        break;
                    }
                }
            } else if Digits::Decimal.value(character).is_some() && exponent_found {
                taken = true;
                if !self.advance() {
                    break;
                }
            } else {
                break;
            }
        }
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::Cursor;
    use crate::vm::lexer::Value;

    /// The value of the number literal the text holds.
    fn value(text: &str) -> Value {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut cursor = Cursor::new(units);
        cursor.scan_number().expect("the text is a number literal")
    }

    /// A decimal literal whose exponent mark carries no digit keeps its
    /// digits, and the point no longer scales them.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsLex.cpp:382-432 — the mark and the
    /// sign that may follow it stay in the literal's text.
    /// reference/krkr2/cpp/core/tjs2/tjsLex.cpp:593-596 — the text is the
    /// number conversion's input.
    /// reference/krkr2/cpp/core/tjs2/tjsLex.cpp:684-697 — a text with a
    /// point or a mark is a real.
    /// reference/krkr2/cpp/core/tjs2/tjsConfig.cpp:801-804 — a mark with no
    /// digit leaves the conversion at its `done` label, so neither the
    /// exponent nor the scale the point contributes is applied.
    #[test]
    fn an_exponent_with_no_digit_keeps_the_digits_unscaled() {
        assert_eq!(value("1.5e"), Value::Real(15.0));
        assert_eq!(value(".5e"), Value::Real(5.0));
        assert_eq!(value("12e-"), Value::Real(12.0));
        assert_eq!(value("12.5e"), Value::Real(125.0));
    }

    /// A literal's text is read as the number it writes, whichever base its
    /// prefix names.
    ///
    /// reference/krkr2/cpp/core/tjs2/tjsLex.cpp:644-697 — the prefix picks
    /// the base and the digits, and the decimal branch reads the rest.
    #[test]
    fn an_ordinary_literal_is_the_number_it_writes() {
        assert_eq!(value("1.5"), Value::Real(1.5));
        assert_eq!(value("1e3"), Value::Real(1000.0));
        assert_eq!(value("12"), Value::Integer(12));
        assert_eq!(value("0x1f"), Value::Integer(31));
        assert_eq!(value("0b1011"), Value::Integer(11));
        assert_eq!(value("07"), Value::Integer(7));
    }
}
