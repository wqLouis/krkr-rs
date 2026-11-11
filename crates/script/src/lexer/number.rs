//! The number literals.
//!
//! A number is scanned in two steps: the text of the literal is collected,
//! and the collected text is converted to a value. The two steps are kept
//! apart because the same text is read twice — once to find where the
//! literal ends, and once to compute its value.

use super::{Cursor, Digits, Literal};

impl Cursor {
    /// The number literal at the current position.
    ///
    /// A leading sign is part of the literal. `None` means the text at the
    /// cursor is not a number.
    pub(super) fn scan_number(&mut self) -> Option<Literal> {
        let negative = match self.current() {
            Some(character) if character == u16::from(b'+') => {
                if !self.advance() || !self.skip_space() {
                    return None;
                }
                false
            }
            Some(character) if character == u16::from(b'-') => {
                if !self.advance() || !self.skip_space() {
                    return None;
                }
                true
            }
            _ => false,
        };

        let value = self.scan_number_body()?;
        Some(if negative { value.negated() } else { value })
    }

    /// The number literal at the current position, without a sign.
    fn scan_number_body(&mut self) -> Option<Literal> {
        if self.match_word("true") {
            return Some(Literal::Integer(1));
        }
        if self.match_word("false") {
            return Some(Literal::Integer(0));
        }
        if self.match_word("NaN") {
            return Some(Literal::Real(f64::NAN));
        }
        if self.match_word("Infinity") {
            return Some(Literal::Real(f64::INFINITY));
        }

        let start = self.position();
        if self.current() == Some(u16::from(b'0')) {
            if !self.advance() {
                return Some(Literal::Integer(0));
            }
            match self.current() {
                Some(character) if character == u16::from(b'X') || character == u16::from(b'x') => {
                    if !self.advance() {
                        return None;
                    }
                    return self.scan_non_decimal(Digits::Hexadecimal);
                }
                Some(character) if character == u16::from(b'B') || character == u16::from(b'b') => {
                    if !self.advance() {
                        return None;
                    }
                    return self.scan_non_decimal(Digits::Binary);
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
                    return self.scan_non_decimal(Digits::Octal);
                }
            }
        }

        self.scan_decimal()
    }

    /// A decimal number literal at the current position.
    fn scan_decimal(&mut self) -> Option<Literal> {
        let (text, is_real) = self.extract_number(Digits::Decimal, *b"Ee");
        if text.is_empty() {
            return None;
        }
        if is_real {
            return parse_decimal_real(&text).map(Literal::Real);
        }
        let mut value: i64 = 0;
        for &character in &text {
            let digit = Digits::Decimal.value(character)?;
            value = value.wrapping_mul(10).wrapping_add(i64::from(digit));
        }
        Some(Literal::Integer(value))
    }

    /// A hexadecimal, octal or binary number literal at the current
    /// position.
    fn scan_non_decimal(&mut self, digits: Digits) -> Option<Literal> {
        let (text, is_real) = self.extract_number(digits, *b"Pp");
        if text.is_empty() {
            return None;
        }
        if is_real {
            return Some(Literal::Real(parse_non_decimal_real(&text, digits)));
        }
        let bits = digits.bits();
        let mut value: i64 = 0;
        for &character in &text {
            let digit = digits.value(character).unwrap_or(-1);
            value = value.wrapping_shl(bits).wrapping_add(i64::from(digit));
        }
        Some(Literal::Integer(value))
    }

    /// The text of a number literal at the current position, and whether it
    /// has a fractional or an exponent part.
    ///
    /// The current position is left after the literal.
    pub(super) fn extract_number(
        &mut self,
        digits: Digits,
        exponent_marks: [u8; 2],
    ) -> (Vec<u16>, bool) {
        let mut text: Vec<u16> = Vec::new();
        let mut point_found = false;
        let mut exponent_found = false;
        while let Some(character) = self.current() {
            if digits.value(character).is_some() && !exponent_found {
                text.push(character);
                if !self.advance() {
                    break;
                }
            } else if character == u16::from(b'.') && !point_found && !exponent_found {
                point_found = true;
                text.push(character);
                if !self.advance() {
                    break;
                }
            } else if (character == u16::from(exponent_marks[0])
                || character == u16::from(exponent_marks[1]))
                && !exponent_found
            {
                exponent_found = true;
                text.push(character);
                if !self.advance() {
                    break;
                }
                if !self.skip_space() {
                    break;
                }
                if let Some(sign) = self.current()
                    && (sign == u16::from(b'+') || sign == u16::from(b'-'))
                {
                    text.push(sign);
                    if !self.advance() {
                        break;
                    }
                    if !self.skip_space() {
                        break;
                    }
                }
            } else if Digits::Decimal.value(character).is_some() && exponent_found {
                text.push(character);
                if !self.advance() {
                    break;
                }
            } else {
                break;
            }
        }
        (text, point_found || exponent_found)
    }
}

impl Literal {
    /// The value with its sign flipped.
    fn negated(self) -> Self {
        match self {
            Self::Integer(value) => Self::Integer(value.wrapping_neg()),
            Self::Real(value) => Self::Real(-value),
            other => other,
        }
    }
}

/// The value of a decimal real literal.
///
/// The text is read as the longest prefix that is a decimal number: a
/// trailing exponent marker without digits is not part of the number.
///
fn parse_decimal_real(text: &[u16]) -> Option<f64> {
    let prefix = decimal_prefix(text);
    let mut digits = String::with_capacity(prefix.len());
    for &character in prefix {
        digits.push(char::from_u32(u32::from(character))?);
    }
    digits.parse::<f64>().ok()
}

/// The longest prefix of the text that reads as a decimal number.
fn decimal_prefix(text: &[u16]) -> &[u16] {
    let mut index = 0;
    while index < text.len() && Digits::Decimal.value(text[index]).is_some() {
        index += 1;
    }
    let mut end = index;
    if index < text.len() && text[index] == u16::from(b'.') {
        index += 1;
        while index < text.len() && Digits::Decimal.value(text[index]).is_some() {
            index += 1;
        }
        end = index;
    }
    if index < text.len() && (text[index] == u16::from(b'e') || text[index] == u16::from(b'E')) {
        let mut probe = index + 1;
        if probe < text.len() && (text[probe] == u16::from(b'+') || text[probe] == u16::from(b'-'))
        {
            probe += 1;
        }
        let digits_start = probe;
        while probe < text.len() && Digits::Decimal.value(text[probe]).is_some() {
            probe += 1;
        }
        if probe > digits_start {
            end = probe;
        }
    }
    &text[..end]
}

/// The value of a hexadecimal, octal or binary real literal.
///
/// The significand is assembled bit by bit and the exponent is applied to
/// it, so the value is the one the bit layout describes and not the one a
/// decimal conversion would round to.
fn parse_non_decimal_real(text: &[u16], digits: Digits) -> f64 {
    const SIGNIFICAND_BITS: u32 = 52;
    const EXPONENT_BIAS: i32 = 1023;
    const EXPONENT_MIN: i32 = -1022;
    const EXPONENT_MAX: i32 = 1023;

    let bits = digits.bits();
    let mut main: u64 = 0;
    let mut exponent: i32 = 0;
    let mut significand: i32 = 0;
    let mut point_passed = false;
    let mut index = 0;

    while index < text.len() {
        let character = text[index];
        if character == u16::from(b'.') {
            point_passed = true;
        } else if character == u16::from(b'p') || character == u16::from(b'P') {
            index += 1;
            if index >= text.len() {
                break;
            }
            let mut negative = false;
            if text[index] == u16::from(b'+') {
                index += 1;
                if index >= text.len() {
                    break;
                }
            } else if text[index] == u16::from(b'-') {
                negative = true;
                index += 1;
                if index >= text.len() {
                    break;
                }
            }
            let mut bias: i32 = 0;
            while index < text.len() {
                bias = bias.wrapping_mul(10);
                bias = bias.wrapping_add(Digits::Decimal.value(text[index]).unwrap_or(-1));
                index += 1;
            }
            if negative {
                bias = bias.wrapping_neg();
            }
            exponent = exponent.wrapping_add(bias);
            break;
        } else {
            let digit = digits.value(character).unwrap_or(-1);
            if significand == 0 {
                let mut bit = bits as i32 - 1;
                while bit >= 0 {
                    if (1 << bit) & digit != 0 {
                        break;
                    }
                    bit -= 1;
                }
                bit += 1;
                if bit != 0 {
                    significand = bit;
                    main |= (digit as u64) << ((64 - significand) as u32 & 63);
                    if point_passed {
                        exponent = exponent.wrapping_sub(bits as i32 - bit + 1);
                    } else {
                        exponent = bit - 1;
                    }
                } else if point_passed {
                    exponent = exponent.wrapping_sub(bits as i32);
                }
            } else {
                if significand + (bits as i32) < 64 {
                    significand += bits as i32;
                    main |= (digit as u64) << ((64 - significand) as u32 & 63);
                }
                if !point_passed {
                    exponent = exponent.wrapping_add(bits as i32);
                }
            }
        }
        index += 1;
    }

    main >>= 64 - 1 - SIGNIFICAND_BITS;
    if main == 0 {
        return 0.0;
    }
    main &= (1 << SIGNIFICAND_BITS) - 1;
    if exponent < EXPONENT_MIN {
        return 0.0;
    }
    if exponent > EXPONENT_MAX {
        return f64::INFINITY;
    }
    let composed = ((exponent + EXPONENT_BIAS) as u64) << SIGNIFICAND_BITS | main;
    f64::from_bits(composed)
}
