//! The numbers, and the rules that read a number out of text.
//!
//! A script has two numeric kinds: a 64-bit signed integer and a 64-bit real.
//! One routine reads a number's text and decides the kind: a text with a
//! decimal point or an exponent is a real, every other text is an integer.
//! `0x` writes hexadecimal, `0b` binary, a leading `0` octal, and everything
//! else decimal.
//!
//! # The text a number is read from
//!
//! A sign belongs to the number: `"-12"` is minus twelve, and `"+ 12"` is
//! twelve because the spaces after a sign are skipped. A space *before* the
//! number is not skipped, so `" 12"` is not a number at all.
//!
//! A number is read from the start of the text and ends at the first code
//! unit that cannot continue it: [`Parsed::consumed`] says where it ended,
//! and the units after that belong to the caller. A text that does not begin
//! with a number has no value — that outcome is [`NumberError::NotANumber`],
//! never a zero the caller cannot tell from a number that is zero.
//!
//! # The text a number is written as
//!
//! A number writes itself as [`Display`]: an integer in decimal, a real with
//! fifteen significant digits, and the named forms `NaN`, `+Infinity`,
//! `-Infinity`, `+0.0` and `-0.0`.

use std::fmt::{self, Write as _};

/// The number of significant digits a real's text holds.
const REAL_DIGITS: usize = 15;

/// The decimal exponent from which a real is written in the exponent form.
const FIXED_FORM_LOWEST_EXPONENT: i32 = -4;

/// The most characters a real's text holds.
///
/// The longest form is a sign, one integer digit, the point, fourteen
/// fractional digits, the exponent marker, its sign and three exponent
/// digits — `-1.79769313486232e+308` — so the answer is allocated once, at
/// its greatest size, and never grows.
const REAL_TEXT_CAPACITY: usize = 22;

/// The largest number of mantissa digits the real conversion collects.
const MOST_MANTISSA_DIGITS: i32 = 48;

/// The greatest decimal exponent the real conversion applies.
const GREATEST_EXPONENT: u32 = 511;

/// The code unit that marks text a conditional-compilation statement removed.
///
/// A number's text steps over these units, because they stand where no text
/// is.
const SKIP_CODE: u16 = 0xffff;

/// The powers of ten the real conversion scales a value by, as `10^(2^i)`.
const POWERS_OF_TEN: [f64; 9] = [
    10.0, 100.0, 1.0e4, 1.0e8, 1.0e16, 1.0e32, 1.0e64, 1.0e128, 1.0e256,
];

/// The bits a double devotes to its significand.
const SIGNIFICAND_BITS: i32 = 52;

/// The exponent a double's field is biased by.
const EXPONENT_BIAS: i32 = 1023;

/// The lowest exponent a double's field holds.
const EXPONENT_LOWEST: i32 = -1022;

/// The greatest exponent a double's field holds.
const EXPONENT_GREATEST: i32 = 1023;

/// The lowest real that has a 64-bit integer, inclusive.
const LOWEST_INTEGER_REAL: f64 = -9_223_372_036_854_775_808.0;

/// The first real above the 64-bit integer range.
const HIGHEST_INTEGER_REAL: f64 = 9_223_372_036_854_775_808.0;

/// A number: the two kinds the language has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Number {
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit floating-point real.
    Real(f64),
}

impl Number {
    /// The value as an integer, with a real truncated toward zero.
    ///
    /// # Errors
    ///
    /// [`NumberError::RealOutOfRange`] when a real is infinite, not a
    /// number, or outside the range a 64-bit integer holds.
    pub fn to_integer(self) -> Result<i64, NumberError> {
        match self {
            Self::Integer(value) => Ok(value),
            Self::Real(value) => {
                if (LOWEST_INTEGER_REAL..HIGHEST_INTEGER_REAL).contains(&value) {
                    Ok(value as i64)
                } else {
                    Err(NumberError::RealOutOfRange)
                }
            }
        }
    }

    /// The value as a real.
    ///
    /// An integer wider than a real's significand loses its low digits, the
    /// way every widening to a real does.
    pub fn to_real(self) -> f64 {
        match self {
            Self::Integer(value) => value as f64,
            Self::Real(value) => value,
        }
    }

    /// The value with its sign flipped.
    fn negated(self) -> Self {
        match self {
            Self::Integer(value) => Self::Integer(value.wrapping_neg()),
            Self::Real(value) => Self::Real(-value),
        }
    }
}

impl fmt::Display for Number {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Real(value) => formatter.write_str(&real_text(value)),
        }
    }
}

/// A number read from text, and the text it occupies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Parsed {
    /// The value.
    pub number: Number,
    /// The number of code units the number's text occupies, including its
    /// sign and the spaces after that sign.
    pub consumed: usize,
}

/// The failure of a text-to-number conversion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum NumberError {
    /// The text does not begin with a number.
    #[error("the text is not a number")]
    NotANumber,

    /// A real that no 64-bit integer can hold.
    #[error("the real has no 64-bit integer")]
    RealOutOfRange,
}

/// The number the given text holds, and where its text ends.
///
/// # Errors
///
/// [`NumberError::NotANumber`] when the text does not begin with a number.
pub fn parse(units: &[u16]) -> Result<Parsed, NumberError> {
    let mut cursor = Cursor::new(units);
    let negative = match cursor.at() {
        Some(unit) if unit == u16::from(b'+') => {
            if !cursor.advance() || !cursor.skip_space() {
                return Err(NumberError::NotANumber);
            }
            false
        }
        Some(unit) if unit == u16::from(b'-') => {
            if !cursor.advance() || !cursor.skip_space() {
                return Err(NumberError::NotANumber);
            }
            true
        }
        _ => false,
    };
    let number = parse_body(&mut cursor)?;
    Ok(Parsed {
        number: if negative { number.negated() } else { number },
        consumed: cursor.position,
    })
}

/// A position in the text a number is read from.
struct Cursor<'a> {
    /// The code units.
    units: &'a [u16],
    /// The unit the cursor is at.
    position: usize,
}

impl<'a> Cursor<'a> {
    /// The cursor at the start of the text.
    fn new(units: &'a [u16]) -> Self {
        Self { units, position: 0 }
    }

    /// The unit at the cursor, or `None` at the end of the text.
    fn at(&self) -> Option<u16> {
        self.units.get(self.position).copied()
    }

    /// The number of code units the cursor has left.
    ///
    /// The extracted text is a subsequence of them, so this bounds its
    /// length; a number that ends the text (the usual case) gives it
    /// exactly.
    fn remaining(&self) -> usize {
        self.units.len() - self.position
    }

    /// The sign the cursor is at, if it is at one.
    fn sign(&self) -> Option<u16> {
        match self.at() {
            Some(unit) if unit == u16::from(b'+') || unit == u16::from(b'-') => Some(unit),
            _ => None,
        }
    }

    /// Moves the cursor one unit on, stepping over the skipped code units,
    /// and answers whether a unit follows.
    ///
    /// At the end of the text the cursor stays where it is.
    fn advance(&mut self) -> bool {
        if self.position < self.units.len() {
            self.position += 1;
        }
        while self.at() == Some(SKIP_CODE) {
            self.position += 1;
        }
        self.position < self.units.len()
    }

    /// Moves the cursor past the spaces and the skipped code units, and
    /// answers whether a unit follows.
    fn skip_space(&mut self) -> bool {
        while let Some(unit) = self.at() {
            if unit != SKIP_CODE && !is_space(unit) {
                break;
            }
            self.position += 1;
        }
        self.position < self.units.len()
    }
}

/// The number at the cursor, without its sign.
fn parse_body(cursor: &mut Cursor) -> Result<Number, NumberError> {
    if match_word(cursor, b"true") {
        return Ok(Number::Integer(1));
    }
    if match_word(cursor, b"false") {
        return Ok(Number::Integer(0));
    }
    if match_word(cursor, b"NaN") {
        return Ok(Number::Real(f64::NAN));
    }
    if match_word(cursor, b"Infinity") {
        return Ok(Number::Real(f64::INFINITY));
    }

    let start = cursor.position;
    if cursor.at() == Some(u16::from(b'0')) {
        if !cursor.advance() {
            return Ok(Number::Integer(0));
        }
        let mark = cursor.at();
        if mark == Some(u16::from(b'X')) || mark == Some(u16::from(b'x')) {
            if !cursor.advance() {
                return Err(NumberError::NotANumber);
            }
            return parse_non_decimal(cursor, Digits::Hexadecimal);
        }
        if mark == Some(u16::from(b'B')) || mark == Some(u16::from(b'b')) {
            if !cursor.advance() {
                return Err(NumberError::NotANumber);
            }
            return parse_non_decimal(cursor, Digits::Binary);
        }
        // a number whose exponent is a power of two is not written
        if mark == Some(u16::from(b'P')) || mark == Some(u16::from(b'p')) {
            return Err(NumberError::NotANumber);
        }
        cursor.position = start;
        if mark != Some(u16::from(b'.'))
            && mark != Some(u16::from(b'E'))
            && mark != Some(u16::from(b'e'))
        {
            return parse_non_decimal(cursor, Digits::Octal);
        }
    }
    parse_decimal(cursor)
}

/// The decimal number at the cursor.
fn parse_decimal(cursor: &mut Cursor) -> Result<Number, NumberError> {
    let (text, is_real) = extract(cursor, decimal_digit, *b"Ee");
    if text.is_empty() {
        return Err(NumberError::NotANumber);
    }
    if is_real {
        Ok(Number::Real(decimal_real(&text)))
    } else {
        parse_decimal_integer(&text).map(Number::Integer)
    }
}

/// The number of the given non-decimal base at the cursor.
fn parse_non_decimal(cursor: &mut Cursor, digits: Digits) -> Result<Number, NumberError> {
    let (text, is_real) = extract(cursor, |unit| digits.value(unit), *b"Pp");
    if text.is_empty() {
        return Err(NumberError::NotANumber);
    }
    if is_real {
        non_decimal_real(&text, digits).map(Number::Real)
    } else {
        non_decimal_integer(&text, digits).map(Number::Integer)
    }
}

/// The text of the number at the cursor, and whether that text has a
/// fractional or an exponent part.
///
/// The text holds the digits of the given base before an exponent mark, the
/// point, and decimal digits after the mark. The cursor is left after the
/// text.
fn extract(
    cursor: &mut Cursor,
    digit: impl Fn(u16) -> Option<u8>,
    exponent_marks: [u8; 2],
) -> (Vec<u16>, bool) {
    let mut text: Vec<u16> = Vec::with_capacity(cursor.remaining());
    let mut point_found = false;
    let mut exponent_found = false;
    while let Some(character) = cursor.at() {
        if !exponent_found && digit(character).is_some() {
            text.push(character);
            if !cursor.advance() {
                break;
            }
        } else if !point_found && !exponent_found && character == u16::from(b'.') {
            point_found = true;
            text.push(character);
            if !cursor.advance() {
                break;
            }
        } else if !exponent_found
            && (character == u16::from(exponent_marks[0])
                || character == u16::from(exponent_marks[1]))
        {
            exponent_found = true;
            text.push(character);
            if !cursor.advance() {
                break;
            }
            if !cursor.skip_space() {
                break;
            }
            if let Some(sign) = cursor.sign() {
                text.push(sign);
                if !cursor.advance() {
                    break;
                }
                if !cursor.skip_space() {
                    break;
                }
            }
        } else if exponent_found && decimal_digit(character).is_some() {
            text.push(character);
            if !cursor.advance() {
                break;
            }
        } else {
            break;
        }
    }
    (text, point_found || exponent_found)
}

/// The integer the digits of a decimal text hold.
///
/// The digits accumulate in a wrapping 64-bit value, so a text wider than
/// the value keeps its low 64 bits.
fn parse_decimal_integer(text: &[u16]) -> Result<i64, NumberError> {
    let mut value: i64 = 0;
    for &character in text {
        let digit = decimal_digit(character).ok_or(NumberError::NotANumber)?;
        value = value.wrapping_mul(10).wrapping_add(i64::from(digit));
    }
    Ok(value)
}

/// The integer the digits of a non-decimal text hold.
///
/// The digits accumulate in a wrapping 64-bit value, one base digit's worth
/// of bits at a time, so a text wider than the value keeps its low 64 bits.
fn non_decimal_integer(text: &[u16], digits: Digits) -> Result<i64, NumberError> {
    let mut value: i64 = 0;
    for &character in text {
        let digit = digits.value(character).ok_or(NumberError::NotANumber)?;
        value = value
            .wrapping_shl(u32::from(digits.bits()))
            .wrapping_add(i64::from(digit));
    }
    Ok(value)
}

/// The real the text of a decimal real holds.
///
/// The text holds a mantissa — digits and at most one point — and at most one
/// exponent mark with a sign and digits. The value is that of the digits
/// scaled by a power of ten, and it is not the correctly rounded value: the
/// fraction is a double before it is scaled, and the last digits past the
/// first eighteen do not change it.
fn decimal_real(text: &[u16]) -> f64 {
    let mut index = 0;
    // the leading spaces and the sign
    while is_space(unit_at(text, index)) {
        index += 1;
    }
    let negative = unit_at(text, index) == u16::from(b'-');
    if negative || unit_at(text, index) == u16::from(b'+') {
        index += 1;
    }
    // the mantissa: the digits and the point
    let mantissa_start = index;
    let mut decimal_point: i32 = -1;
    let mut mantissa_size: i32 = 0;
    while index < text.len() {
        let character = text[index];
        if decimal_digit(character).is_none() {
            if character != u16::from(b'.') || decimal_point >= 0 {
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
        mantissa_size -= 1;
    }
    // the digits past the first 48 do not change the value
    let (fraction_exponent, mantissa_size) = if mantissa_size > MOST_MANTISSA_DIGITS {
        (decimal_point - MOST_MANTISSA_DIGITS, MOST_MANTISSA_DIGITS)
    } else {
        (decimal_point - mantissa_size, mantissa_size)
    };
    if mantissa_size == 0 {
        return if negative { -0.0 } else { 0.0 };
    }
    // the digits are collected into two integers, the first ones and the
    // last ones
    let mut cursor = mantissa_start;
    let mut remaining = mantissa_size;
    let mut first: i32 = 0;
    while remaining > 9 {
        let mut character = text[cursor];
        cursor += 1;
        if character == u16::from(b'.') {
            character = text[cursor];
            cursor += 1;
        }
        first = first
            .wrapping_mul(10)
            .wrapping_add(i32::from(character - u16::from(b'0')));
        remaining -= 1;
    }
    let mut last: i32 = 0;
    while remaining > 0 {
        let mut character = text[cursor];
        cursor += 1;
        if character == u16::from(b'.') {
            character = text[cursor];
            cursor += 1;
        }
        last = last
            .wrapping_mul(10)
            .wrapping_add(i32::from(character - u16::from(b'0')));
        remaining -= 1;
    }
    let mut fraction = 1.0e9 * f64::from(first) + f64::from(last);
    // the exponent
    let mut exponent: i32 = 0;
    let mut exponent_negative = false;
    let mut cursor = exponent_mark;
    let mark = unit_at(text, cursor);
    if mark == u16::from(b'E') || mark == u16::from(b'e') {
        cursor += 1;
        let sign = unit_at(text, cursor);
        if sign == u16::from(b'-') {
            exponent_negative = true;
            cursor += 1;
        } else if sign == u16::from(b'+') {
            cursor += 1;
        }
        if decimal_digit(unit_at(text, cursor)).is_none() {
            // the exponent carries no digits, so the digits are the value
            return if negative { -fraction } else { fraction };
        }
        while let Some(digit) = decimal_digit(unit_at(text, cursor)) {
            exponent = exponent.wrapping_mul(10).wrapping_add(i32::from(digit));
            cursor += 1;
        }
    }
    let exponent = if exponent_negative {
        fraction_exponent.wrapping_sub(exponent)
    } else {
        fraction_exponent.wrapping_add(exponent)
    };
    // the exponent is applied by the powers of ten: the greatest magnitude
    // the conversion uses is 511
    let exponent_negative = exponent < 0;
    let mut magnitude = exponent.unsigned_abs().min(GREATEST_EXPONENT);
    let mut scale = 1.0f64;
    let mut power = 0usize;
    while magnitude != 0 {
        if magnitude & 1 != 0 {
            scale *= POWERS_OF_TEN[power];
        }
        magnitude >>= 1;
        power += 1;
    }
    if exponent_negative {
        fraction /= scale;
    } else {
        fraction *= scale;
    }
    if negative { -fraction } else { fraction }
}

/// The real the text of a non-decimal real holds.
///
/// The significand is assembled bit by bit from the digits of the base and
/// the exponent is applied to it, so the value is the one the bits describe.
fn non_decimal_real(text: &[u16], digits: Digits) -> Result<f64, NumberError> {
    let base_bits = i32::from(digits.bits());
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
            while index < text.len() && is_space(text[index]) {
                index += 1;
            }
            if index >= text.len() {
                break;
            }
            let mut negative = false;
            if text[index] == u16::from(b'+') {
                index += 1;
            } else if text[index] == u16::from(b'-') {
                negative = true;
                index += 1;
            }
            while index < text.len() && is_space(text[index]) {
                index += 1;
            }
            let mut bias: i32 = 0;
            while index < text.len() {
                let digit = decimal_digit(text[index]).ok_or(NumberError::NotANumber)?;
                bias = bias.wrapping_mul(10).wrapping_add(i32::from(digit));
                index += 1;
            }
            if negative {
                bias = bias.wrapping_neg();
            }
            exponent = exponent.wrapping_add(bias);
            break;
        } else {
            let digit = u64::from(digits.value(character).ok_or(NumberError::NotANumber)?);
            if significand == 0 {
                // the first digit that is not zero is placed at the top
                let mut bit = base_bits - 1;
                while bit >= 0 {
                    if (1 << bit) & digit != 0 {
                        break;
                    }
                    bit -= 1;
                }
                bit += 1;
                if bit != 0 {
                    significand = bit;
                    main |= digit << ((64 - significand) as u32);
                    if point_passed {
                        exponent = exponent.wrapping_sub(base_bits - bit + 1);
                    } else {
                        exponent = bit - 1;
                    }
                } else if point_passed {
                    exponent = exponent.wrapping_sub(base_bits);
                }
            } else {
                if significand + base_bits < 64 {
                    significand += base_bits;
                    main |= digit << ((64 - significand) as u32);
                }
                if !point_passed {
                    exponent = exponent.wrapping_add(base_bits);
                }
            }
        }
        index += 1;
    }
    let mut bits = main >> (64 - 1 - SIGNIFICAND_BITS);
    if bits == 0 {
        return Ok(0.0);
    }
    bits &= (1 << SIGNIFICAND_BITS) - 1;
    if exponent < EXPONENT_LOWEST {
        return Ok(0.0);
    }
    if exponent > EXPONENT_GREATEST {
        return Ok(f64::INFINITY);
    }
    // the exponent field holds a value the range above leaves non-negative
    let composed = ((exponent + EXPONENT_BIAS) as u64) << SIGNIFICAND_BITS;
    Ok(f64::from_bits(composed | bits))
}

/// The text a real is written as.
///
/// The text holds fifteen significant digits: the exponent form when the
/// decimal exponent is below `-4` or at least the number of digits, and the
/// plain form otherwise, with the insignificant zero digits dropped.
///
/// The digits are collected as the formatter emits them (see
/// [`Significand`]) and written into the answer in one pass, so no text
/// exists between the formatter and the answer.
fn real_text(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 {
            "-Infinity"
        } else {
            "+Infinity"
        }
        .to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "+0.0"
        }
        .to_owned();
    }
    let rounded = rounded_digits(value.abs());
    let digits = &rounded.digits[..rounded.count];
    // the insignificant zero digits are not written
    let mut significant = digits.len();
    while significant > 1 && digits[significant - 1] == b'0' {
        significant -= 1;
    }
    let digits = &digits[..significant];
    let exponent = rounded.exponent;
    let mut text = String::with_capacity(REAL_TEXT_CAPACITY);
    if value < 0.0 {
        text.push('-');
    }
    if exponent < FIXED_FORM_LOWEST_EXPONENT || exponent >= REAL_DIGITS as i32 {
        // one digit, the rest of the digits after the point, and the
        // exponent with a sign and at least two digits
        push_digits(&mut text, &digits[..1]);
        if digits.len() > 1 {
            text.push('.');
            push_digits(&mut text, &digits[1..]);
        }
        text.push('e');
        text.push(if exponent < 0 { '-' } else { '+' });
        push_magnitude(&mut text, exponent.unsigned_abs(), 2);
    } else if exponent >= 0 {
        // the point falls inside the significand or just after it
        let point = exponent as usize + 1;
        if digits.len() <= point {
            push_digits(&mut text, digits);
            for _ in digits.len()..point {
                text.push('0');
            }
        } else {
            push_digits(&mut text, &digits[..point]);
            text.push('.');
            push_digits(&mut text, &digits[point..]);
        }
    } else {
        text.push_str("0.");
        for _ in 0..(-exponent - 1) {
            text.push('0');
        }
        push_digits(&mut text, digits);
    }
    text
}

/// The rounded significand digits of a positive real, and the decimal
/// exponent of its first digit.
///
/// The value is rounded to [`REAL_DIGITS`] significant digits, and the
/// digits are the rounded significand from its first digit, which a value
/// that is not zero never leaves at zero.
fn rounded_digits(value: f64) -> Significand {
    let mut digits = Significand::default();
    write!(&mut digits, "{:.*e}", REAL_DIGITS - 1, value).expect("the digit sink never fails");
    if digits.negative {
        digits.exponent = -digits.exponent;
    }
    digits
}

/// The significand of a real, as the formatter rounds it.
///
/// The formatter emits the rounded value as its digits, a point, the
/// exponent marker and the exponent; this sink keeps the digits and the
/// exponent as it reads them, so the value's digits exist as numbers and
/// never as text.
#[derive(Default)]
struct Significand {
    /// The significand digits, most significant first.
    digits: [u8; REAL_DIGITS],
    /// The number of digits kept.
    count: usize,
    /// The decimal exponent of the first digit.
    exponent: i32,
    /// Whether the exponent carries a minus sign.
    negative: bool,
    /// Whether the exponent marker has been read.
    in_exponent: bool,
}

impl fmt::Write for Significand {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for byte in text.bytes() {
            if byte == b'e' {
                self.in_exponent = true;
            } else if self.in_exponent {
                if byte == b'-' {
                    self.negative = true;
                } else if byte.is_ascii_digit() {
                    self.exponent = self.exponent * 10 + i32::from(byte - b'0');
                }
            } else if byte.is_ascii_digit() && self.count < REAL_DIGITS {
                self.digits[self.count] = byte;
                self.count += 1;
            }
        }
        Ok(())
    }
}

/// Write digit bytes as characters.
fn push_digits(text: &mut String, digits: &[u8]) {
    for &digit in digits {
        text.push(char::from(digit));
    }
}

/// Write the decimal digits of a magnitude, at least `width` digits wide.
fn push_magnitude(text: &mut String, mut magnitude: u32, width: usize) {
    let mut digits = [0u8; 3];
    let mut count = 0;
    while magnitude != 0 && count < digits.len() {
        digits[count] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        count += 1;
    }
    for _ in count..width {
        text.push('0');
    }
    while count > 0 {
        count -= 1;
        text.push(char::from(digits[count]));
    }
}

/// The code unit at the index, or the zero code unit that ends the text.
///
/// A conversion reads the code unit after the mantissa to find the exponent,
/// and the code unit after the exponent's digits to find their end; the zero
/// is the end of the text there.
fn unit_at(text: &[u16], index: usize) -> u16 {
    match text.get(index) {
        Some(&unit) => unit,
        None => 0,
    }
}

/// Whether the code unit is one of the spaces a number skips.
fn is_space(unit: u16) -> bool {
    matches!(unit, 0x09..=0x0d | 0x20)
}

/// Whether the code unit continues a word: a letter beyond the basic range,
/// an ASCII letter, or the underscore.
fn is_word_continuation(unit: u16) -> bool {
    unit >= 0x100 || unit == u16::from(b'_') || matches!(unit, 0x41..=0x5a | 0x61..=0x7a)
}

/// The value of a decimal digit code unit, or `None`.
fn decimal_digit(unit: u16) -> Option<u8> {
    match unit {
        0x30..=0x39 => Some((unit - 0x30) as u8),
        _ => None,
    }
}

/// The word at the cursor, which must be a whole word.
///
/// The cursor is left after the word when it matches, and where it was when
/// it does not.
fn match_word(cursor: &mut Cursor, word: &[u8]) -> bool {
    let start = cursor.position;
    for &byte in word {
        if cursor.at() != Some(u16::from(byte)) {
            cursor.position = start;
            return false;
        }
        cursor.advance();
    }
    match cursor.at() {
        Some(unit) if is_word_continuation(unit) => {
            cursor.position = start;
            false
        }
        _ => true,
    }
}

/// The digits a non-decimal number is written in.
#[derive(Clone, Copy)]
enum Digits {
    /// Base two: the digits `0` and `1`.
    Binary,
    /// Base eight: the digits `0` to `7`.
    Octal,
    /// Base sixteen: the digits `0` to `9` and `a` to `f` in either case.
    Hexadecimal,
}

impl Digits {
    /// The value of a digit code unit, or `None` when the unit is not a
    /// digit of this base.
    fn value(self, unit: u16) -> Option<u8> {
        match self {
            Self::Binary => match unit {
                0x30 => Some(0),
                0x31 => Some(1),
                _ => None,
            },
            Self::Octal => match unit {
                0x30..=0x37 => Some((unit - 0x30) as u8),
                _ => None,
            },
            Self::Hexadecimal => match unit {
                0x30..=0x39 => Some((unit - 0x30) as u8),
                0x41..=0x46 => Some((unit - 0x41 + 10) as u8),
                0x61..=0x66 => Some((unit - 0x61 + 10) as u8),
                _ => None,
            },
        }
    }

    /// The number of bits one digit of this base stands for.
    fn bits(self) -> u8 {
        match self {
            Self::Binary => 1,
            Self::Octal => 3,
            Self::Hexadecimal => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Number, NumberError, parse};

    /// The code units of a text.
    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// The number a text holds.
    fn number(text: &str) -> Result<Number, NumberError> {
        parse(&units(text)).map(|parsed| parsed.number)
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:611-697
    // reference/cpp/core/tjs2/tjsLex.cpp:560-572
    // reference/cpp/core/tjs2/tjsLex.cpp:598-609
    #[test]
    fn the_integer_forms() {
        assert_eq!(number("0"), Ok(Number::Integer(0)));
        assert_eq!(number("-0"), Ok(Number::Integer(0)));
        assert_eq!(number("1234567890"), Ok(Number::Integer(1_234_567_890)));
        // the digits accumulate in a wrapping 64-bit value
        assert_eq!(
            number("12345678901234567890"),
            Ok(Number::Integer(-6_101_065_172_474_983_726))
        );
        // one above the greatest integer reads back as the lowest integer
        assert_eq!(number("9223372036854775808"), Ok(Number::Integer(i64::MIN)));
        assert_eq!(
            number("-9223372036854775808"),
            Ok(Number::Integer(i64::MIN))
        );
        assert_eq!(number("0x1f"), Ok(Number::Integer(31)));
        assert_eq!(number("0X1F"), Ok(Number::Integer(31)));
        assert_eq!(number("0b11"), Ok(Number::Integer(3)));
        assert_eq!(number("0B11"), Ok(Number::Integer(3)));
        assert_eq!(number("010"), Ok(Number::Integer(8)));
        // `8` is not an octal digit, so the number is the `0` before it
        assert_eq!(number("08"), Ok(Number::Integer(0)));
        assert_eq!(number("0x"), Err(NumberError::NotANumber));
        assert_eq!(number("0b"), Err(NumberError::NotANumber));
        assert_eq!(number("0p3"), Err(NumberError::NotANumber));
        assert_eq!(number("0xg"), Err(NumberError::NotANumber));
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:593-597
    // reference/cpp/core/tjs2/tjsConfig.cpp:672
    // reference/cpp/core/tjs2/tjsConfig.cpp:789-814
    #[test]
    fn the_real_forms() {
        assert_eq!(number("1e3"), Ok(Number::Real(1000.0)));
        assert_eq!(number("1E-3"), Ok(Number::Real(0.001)));
        assert_eq!(number("12.5"), Ok(Number::Real(12.5)));
        assert_eq!(number("12."), Ok(Number::Real(12.0)));
        assert_eq!(number(".5"), Ok(Number::Real(0.5)));
        assert_eq!(number("0.5"), Ok(Number::Real(0.5)));
        assert_eq!(number("."), Ok(Number::Real(0.0)));
        assert_eq!(number("1e-5"), Ok(Number::Real(1e-5)));
        // the spaces after the exponent marker are skipped
        assert_eq!(number("1e 3"), Ok(Number::Real(1000.0)));
        // an exponent marker with no digits leaves the digits unscaled
        assert_eq!(number("12.5e"), Ok(Number::Real(125.0)));
        assert_eq!(number("0.5e"), Ok(Number::Real(5.0)));
        assert_eq!(number("12e-"), Ok(Number::Real(12.0)));
        // a marker with no digits before it has no digits to scale
        assert_eq!(number("e5"), Ok(Number::Real(0.0)));
    }

    // reference/cpp/core/tjs2/tjsConfig.cpp:693
    // reference/cpp/core/tjs2/tjsConfig.cpp:816-840
    #[test]
    fn an_exponent_beyond_the_range_is_infinity_or_zero() {
        assert_eq!(number("1e400"), Ok(Number::Real(f64::INFINITY)));
        let Number::Real(underflow) = number("1e-400").expect("the text is a number") else {
            panic!("the text is a real");
        };
        assert_eq!(underflow, 0.0);
        assert!(underflow.is_sign_positive());
        let Number::Real(negative_underflow) = number("-1e-400").expect("the text is a number")
        else {
            panic!("the text is a real");
        };
        assert_eq!(negative_underflow, 0.0);
        assert!(negative_underflow.is_sign_negative());
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:611-637
    // reference/cpp/core/tjs2/tjsLex.cpp:216-241
    #[test]
    fn the_named_numbers() {
        assert_eq!(number("true"), Ok(Number::Integer(1)));
        assert_eq!(number("false"), Ok(Number::Integer(0)));
        assert_eq!(number("Infinity"), Ok(Number::Real(f64::INFINITY)));
        assert_eq!(number("-Infinity"), Ok(Number::Real(f64::NEG_INFINITY)));
        let not_a_number = number("NaN").expect("the text is a number");
        assert!(not_a_number.to_real().is_nan());
        // the name is a whole word, but a digit does not continue a word
        assert_eq!(number("true1"), Ok(Number::Integer(1)));
        assert_eq!(number("trueX"), Err(NumberError::NotANumber));
        assert_eq!(number("inf"), Err(NumberError::NotANumber));
        assert_eq!(number("nan"), Err(NumberError::NotANumber));
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:700-727
    #[test]
    fn a_text_that_is_not_a_number() {
        assert_eq!(number(""), Err(NumberError::NotANumber));
        assert_eq!(number("+"), Err(NumberError::NotANumber));
        assert_eq!(number("-"), Err(NumberError::NotANumber));
        // the space before the number is not skipped
        assert_eq!(number(" 12 "), Err(NumberError::NotANumber));
        assert_eq!(number("abc"), Err(NumberError::NotANumber));
        assert_eq!(number("0x"), Err(NumberError::NotANumber));
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:382-429
    // reference/cpp/core/tjs2/tjsLex.cpp:700-720
    #[test]
    fn the_number_ends_at_the_first_other_unit() {
        assert_eq!(parse(&units("12abc")).map(|parsed| parsed.consumed), Ok(2));
        assert_eq!(parse(&units("1e3x")).map(|parsed| parsed.consumed), Ok(3));
        assert_eq!(parse(&units("0x1fz")).map(|parsed| parsed.consumed), Ok(4));
        assert_eq!(parse(&units("0x1p3x")).map(|parsed| parsed.consumed), Ok(5));
        assert_eq!(parse(&units("12 ")).map(|parsed| parsed.consumed), Ok(2));
        // the sign and the spaces after it belong to the number
        assert_eq!(parse(&units("+ 12")).map(|parsed| parsed.consumed), Ok(4));
        assert_eq!(parse(&units("1e 3")).map(|parsed| parsed.consumed), Ok(4));
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:434-558
    #[test]
    fn a_non_decimal_real() {
        assert_eq!(number("0x1p3"), Ok(Number::Real(8.0)));
        assert_eq!(number("0X1P3"), Ok(Number::Real(8.0)));
        assert_eq!(number("0x1.8p1"), Ok(Number::Real(3.0)));
        assert_eq!(number("0x.8"), Ok(Number::Real(0.5)));
        assert_eq!(number("0b1.1"), Ok(Number::Real(1.5)));
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:63-86
    #[test]
    fn the_skipped_code_units_are_stepped_over() {
        assert_eq!(number("1\u{ffff}2"), Ok(Number::Integer(12)));
        assert_eq!(number("1e\u{ffff}3"), Ok(Number::Real(1000.0)));
    }

    // reference/cpp/core/tjs2/tjsConfig.cpp:177-201
    #[test]
    fn an_integer_is_written_in_decimal() {
        assert_eq!(Number::Integer(0).to_string(), "0");
        assert_eq!(Number::Integer(-5).to_string(), "-5");
        assert_eq!(Number::Integer(1_234_567_890).to_string(), "1234567890");
        assert_eq!(Number::Integer(i64::MAX).to_string(), "9223372036854775807");
        assert_eq!(
            Number::Integer(i64::MIN).to_string(),
            "-9223372036854775808"
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:241-249
    #[test]
    fn a_real_is_written_with_fifteen_digits() {
        assert_eq!(Number::Real(1.0).to_string(), "1");
        assert_eq!(Number::Real(0.5).to_string(), "0.5");
        assert_eq!(Number::Real(100.0).to_string(), "100");
        assert_eq!(Number::Real(0.0001).to_string(), "0.0001");
        assert_eq!(Number::Real(1e-5).to_string(), "1e-05");
        assert_eq!(Number::Real(1e15).to_string(), "1e+15");
        assert_eq!(Number::Real(1e16).to_string(), "1e+16");
        assert_eq!(
            Number::Real(1.234567890123456e15).to_string(),
            "1.23456789012346e+15"
        );
        assert_eq!(Number::Real(f64::MAX).to_string(), "1.79769313486232e+308");
        assert_eq!(Number::Real(-2.5).to_string(), "-2.5");
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:219-240
    #[test]
    fn the_special_reals_are_named() {
        assert_eq!(Number::Real(f64::NAN).to_string(), "NaN");
        assert_eq!(Number::Real(f64::INFINITY).to_string(), "+Infinity");
        assert_eq!(Number::Real(f64::NEG_INFINITY).to_string(), "-Infinity");
        assert_eq!(Number::Real(0.0).to_string(), "+0.0");
        assert_eq!(Number::Real(-0.0).to_string(), "-0.0");
    }

    // reference/cpp/core/tjs2/tjsLex.cpp:700-727
    // reference/cpp/core/tjs2/tjsVariant.cpp:241-249
    #[test]
    fn the_text_round_trips() {
        for value in [0, 1, -1, 42, i64::MAX, i64::MIN] {
            let text = Number::Integer(value).to_string();
            assert_eq!(number(&text), Ok(Number::Integer(value)), "{text}");
        }
        // a real's text is the text of the value that text reads back as
        for value in [0.5, -0.5, 1.0 / 3.0, 1e-5, 1e15] {
            let text = Number::Real(value).to_string();
            let read = number(&text).expect("the text is a number");
            assert_eq!(read.to_string(), text, "{text}");
        }
        assert_eq!(number("0.5"), Ok(Number::Real(0.5)));
        // fifteen digits do not hold the greatest real, so its text reads
        // back as the value above it
        assert_eq!(
            number("1.79769313486232e+308"),
            Ok(Number::Real(f64::INFINITY))
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.h:862-880
    #[test]
    fn a_real_is_truncated_toward_zero() {
        assert_eq!(Number::Real(1.9).to_integer(), Ok(1));
        assert_eq!(Number::Real(-1.9).to_integer(), Ok(-1));
        assert_eq!(Number::Real(0.0).to_integer(), Ok(0));
        assert_eq!(Number::Real(-0.0).to_integer(), Ok(0));
        assert_eq!(Number::Integer(i64::MIN).to_integer(), Ok(i64::MIN));
        assert_eq!(
            Number::Real(1e19).to_integer(),
            Err(NumberError::RealOutOfRange)
        );
        assert_eq!(
            Number::Real(f64::NAN).to_integer(),
            Err(NumberError::RealOutOfRange)
        );
        assert_eq!(
            Number::Real(f64::INFINITY).to_integer(),
            Err(NumberError::RealOutOfRange)
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.h:937-955
    #[test]
    fn an_integer_is_widened_to_a_real() {
        assert_eq!(Number::Integer(12).to_real(), 12.0);
        assert_eq!(Number::Integer(-12).to_real(), -12.0);
        assert_eq!(Number::Real(0.5).to_real(), 0.5);
    }
}
