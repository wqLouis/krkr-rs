//! The number literals.
//!
//! A number is scanned in two steps: the text of the literal is collected,
//! and the collected text is converted to a value. The two steps are kept
//! apart because the same text is read twice — once to find where the
//! literal ends, and once to compute its value.
//!
//! The conversion of a number text is here too: the variant reads a
//! number out of a string with the same conversion, and the engine keeps
//! it in one place.

use super::{Cursor, Digits, Value};

impl Cursor {
    /// The number literal at the current position.
    ///
    /// A leading sign is part of the literal. `None` means the text at the
    /// cursor is not a number.
    pub(super) fn scan_number(&mut self) -> Option<Value> {
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
    fn scan_number_body(&mut self) -> Option<Value> {
        if self.match_word("true") {
            return Some(Value::Integer(1));
        }
        if self.match_word("false") {
            return Some(Value::Integer(0));
        }
        if self.match_word("NaN") {
            return Some(Value::Real(f64::NAN));
        }
        if self.match_word("Infinity") {
            return Some(Value::Real(f64::INFINITY));
        }

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
    fn scan_decimal(&mut self) -> Option<Value> {
        let (text, is_real) = self.extract_number(Digits::Decimal, *b"Ee");
        if text.is_empty() {
            return None;
        }
        if is_real {
            return parse_decimal_real(&text).map(Value::Real);
        }
        let mut value: i64 = 0;
        for &character in &text {
            let digit = Digits::Decimal.value(character)?;
            value = value.wrapping_mul(10).wrapping_add(i64::from(digit));
        }
        Some(Value::Integer(value))
    }

    /// A hexadecimal, octal or binary number literal at the current
    /// position.
    fn scan_non_decimal(&mut self, digits: Digits) -> Option<Value> {
        let (text, is_real) = self.extract_number(digits, *b"Pp");
        if text.is_empty() {
            return None;
        }
        if is_real {
            return Some(Value::Real(parse_non_decimal_real(&text, digits)));
        }
        let bits = digits.bits();
        let mut value: i64 = 0;
        for &character in &text {
            let digit = digits.value(character).unwrap_or(-1);
            value = value.wrapping_shl(bits).wrapping_add(i64::from(digit));
        }
        Some(Value::Integer(value))
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

/// The value of a decimal real literal.
///
/// The text is read as the whole number: the exponent marker and its
/// sign are part of the number, even when no digit follows them.
///
/// The value is the engine's own: the conversion is the one the engine
/// uses, and it is not the correctly rounded one.
fn parse_decimal_real(text: &[u16]) -> Option<f64> {
    let mut digits = String::with_capacity(text.len());
    for &character in text {
        digits.push(char::from_u32(u32::from(character))?);
    }
    Some(engine_real(&digits))
}

/// The value of a decimal number, the way the engine reads it.
///
/// The engine's own conversion: the digits of the mantissa are collected
/// into two integers, the fraction is scaled by a power of ten, and the
/// value is the engine's. The conversion is not the correctly rounded
/// one — the fraction is a double before it is scaled — and the engine's
/// value is the one a script sees.
pub(crate) fn engine_real(text: &str) -> f64 {
    /// The powers of ten the exponent is made of.
    const POWERS_OF_TEN: [f64; 9] = [
        10.0, 100.0, 1.0e4, 1.0e8, 1.0e16, 1.0e32, 1.0e64, 1.0e128, 1.0e256,
    ];
    /// The greatest exponent the conversion uses.
    const MAX_EXPONENT: u32 = 511;

    let bytes = text.as_bytes();
    let mut index = 0;
    // the leading blanks and the sign
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let negative = match bytes.get(index).copied() {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    // the mantissa: the digits and the point
    let mantissa_start = index;
    let mut decimal_point: i32 = -1;
    let mut mantissa_size: i32 = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if !byte.is_ascii_digit() {
            if byte != b'.' || decimal_point >= 0 {
                break;
            }
            decimal_point = mantissa_size;
        }
        index += 1;
        mantissa_size += 1;
    }
    let exponent_mark = index;
    if decimal_point < 0 {
        decimal_point = mantissa_size;
    } else {
        // the point is not a digit
        mantissa_size -= 1;
    }
    // the digits past the fortyeighth do not change the value
    let fraction_exponent = if mantissa_size > 48 {
        mantissa_size = 48;
        decimal_point - 48
    } else {
        decimal_point - mantissa_size
    };
    if mantissa_size == 0 {
        return if negative { -0.0 } else { 0.0 };
    }
    // the digits are collected into two integers: the first ones and the
    // last ones
    let mut cursor = mantissa_start;
    let mut first: i32 = 0;
    let mut remaining = mantissa_size;
    while remaining > 9 {
        let mut byte = bytes[cursor];
        cursor += 1;
        if byte == b'.' {
            byte = bytes[cursor];
            cursor += 1;
        }
        first = first.wrapping_mul(10).wrapping_add(i32::from(byte - b'0'));
        remaining -= 1;
    }
    let mut last: i32 = 0;
    while remaining > 0 {
        let mut byte = bytes[cursor];
        cursor += 1;
        if byte == b'.' {
            byte = bytes[cursor];
            cursor += 1;
        }
        last = last.wrapping_mul(10).wrapping_add(i32::from(byte - b'0'));
        remaining -= 1;
    }
    let mut fraction = (1.0e9 * f64::from(first)) + f64::from(last);
    // the exponent
    let mut exponent: i32 = 0;
    let mut exponent_negative = false;
    let mut cursor = exponent_mark;
    if matches!(bytes.get(cursor).copied(), Some(b'e' | b'E')) {
        cursor += 1;
        match bytes.get(cursor).copied() {
            Some(b'-') => {
                exponent_negative = true;
                cursor += 1;
            }
            Some(b'+') => cursor += 1,
            _ => {}
        }
        if !bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            // the exponent has no digits: the number has no exponent
            exponent = 0;
            exponent_negative = false;
        } else {
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                exponent = exponent
                    .wrapping_mul(10)
                    .wrapping_add(i32::from(bytes[cursor] - b'0'));
                cursor += 1;
            }
        }
    }
    let exponent = if exponent_negative {
        fraction_exponent.wrapping_sub(exponent)
    } else {
        fraction_exponent.wrapping_add(exponent)
    };
    // the exponent is applied by the powers of ten: the magnitude is the
    // greatest the conversion uses
    let negative_exponent = exponent < 0;
    let mut magnitude = exponent.unsigned_abs().min(MAX_EXPONENT);
    let mut scale = 1.0f64;
    let mut power = 0usize;
    while magnitude != 0 {
        if magnitude & 1 != 0 {
            scale *= POWERS_OF_TEN[power];
        }
        magnitude >>= 1;
        power += 1;
    }
    if negative_exponent {
        fraction /= scale;
    } else {
        fraction *= scale;
    }
    if negative { -fraction } else { fraction }
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

#[cfg(test)]
mod tests {
    use super::engine_real;

    /// A long decimal literal is the engine's number.
    ///
    /// The engine reads a real literal with its own conversion
    /// (`TJS_strtod`, reference/krkr2/cpp/core/tjs2/tjsConfig.cpp:672),
    /// and the conversion is not the correctly rounded one: the fraction
    /// is a double before it is scaled, so the value is one unit in the
    /// last place off.
    #[test]
    fn a_long_decimal_literal_is_the_engines_number() {
        let value = engine_real("9.090909090909093");
        assert_eq!(value.to_bits(), 0x4022_2e8b_a2e8_ba2f);
        // the correctly rounded value is one unit in the last place higher
        assert_eq!(
            "9.090909090909093"
                .parse::<f64>()
                .expect("the text reads as a number")
                .to_bits(),
            0x4022_2e8b_a2e8_ba30
        );
    }

    /// A number with an exponent is the engine's number.
    ///
    /// The engine's conversion reads the exponent and scales the fraction
    /// by a power of ten
    /// (reference/krkr2/cpp/core/tjs2/tjsConfig.cpp:672).
    #[test]
    fn a_number_with_an_exponent_is_the_engines_number() {
        assert_eq!(engine_real("1.5e10"), 1.5e10);
        assert_eq!(engine_real("1.5e-10"), 1.5e-10);
        assert_eq!(engine_real("0.5"), 0.5);
    }
}
