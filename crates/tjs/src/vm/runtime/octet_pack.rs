//! The pack format: a template that turns values into bytes, and bytes back
//! into values.
//!
//! A format is a text of fields. Each field is one character naming the kind
//! of value and an optional decimal count, or `*` for "every value that is
//! left". Packing reads the values from the front and writes their bytes;
//! unpacking reads the bytes and answers the values it built. Nothing in the
//! stream says what it holds: the template is the only description of it, so
//! the same bytes read with two templates answer two different things.
//!
//! The kinds are the ones the format's letters name: an ASCII run (`a`, `A`),
//! a bit run (`b`, `B`), a one-byte number (`c`, `C`), a real (`d`, `f`), a
//! hex run (`h`, `H`), a four-byte number (`i`, `I`, `l`, `L`), a network-order
//! number (`n`, `N`), a little-endian number (`v`, `V`), a sixteen-bit number
//! (`s`, `S`), base64 (`m`), and the three that consume or pad positions (`x`,
//! `X`, `@`). A letter that names no kind is a typed error, and so is a kind
//! the format does not serve.
//!
//! # The rules that are not obvious
//!
//! Packing reads one value per field, and a field with a count of `n` consumes
//! `n` values; a count larger than the values left consumes what is there. A
//! value's number is its own integer conversion, so a text is read as a number
//! and a real is truncated, and an object or an octet raises. The numeric
//! kinds keep the low bytes of that number: `i` writes the low four bytes of
//! the two's-complement value, `c` the low byte.
//!
//! An ASCII run packs each text unit's low byte, not its UTF-8 form; unpacking
//! a run drops zero bytes and reads the rest as UTF-8, so the two directions
//! are not inverses above ASCII. A hex run accepts an upper-case `A` through
//! `E` and a lower-case `a` through `f`; an upper-case `F` is a typed error.
//!
//! `x` writes zero bytes, `X` backs the output up, and `@` pads the output to
//! an absolute length without truncating it. Those three consume no value, so
//! a count of `*` on one of them never advances and is a typed error rather
//! than the loop it would be. A field the template never reaches because the
//! values are exhausted is not performed at all.
//!
//! Base64 packing accepts whole groups of four only; a text of another length
//! is a typed error rather than a read past its end.
//!
//! A packed result with no bytes at all is the null object, not an empty
//! octet: that is what the array member answers, and [`pack`] says so by
//! answering `None`.

use super::octet::Octet;
use super::string::TjsString;
use super::variant::{Variant, VariantError};

/// The failure of a pack or an unpack.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum PackError {
    /// The format names no known kind.
    #[error("the format names no known kind")]
    UnknownTemplate,

    /// A field's count does not fit.
    #[error("the field count is out of range")]
    LengthOutOfRange,

    /// A value cannot be converted to what the field needs.
    #[error("the value cannot be converted")]
    Value(#[from] VariantError),

    /// A bit run holds a character that is not `0` or `1`.
    #[error("the bit run holds a character that is not a bit")]
    BitString,

    /// A hex run holds a character that is not a hex digit.
    #[error("the hex run holds a character that is not a hex digit")]
    HexString,

    /// A backed-up field would remove more bytes than the output holds.
    #[error("the output is shorter than the back-up count")]
    OutputUnderflow,

    /// The field's kind is not served.
    #[error("the field's kind is not served")]
    Unsupported,

    /// The bytes of an ASCII run are not the text encoding they are read as.
    #[error("the bytes are not a valid text")]
    Text,

    /// The base64 text is not a whole number of groups, or a unit is not
    /// ASCII.
    #[error("the base64 text is not valid")]
    Base64,

    /// A `*` count on a field that consumes no value would never advance.
    #[error("the field repeats without consuming a value")]
    NoProgress,
}

/// One kind of field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Code {
    /// `a`: a text's low bytes, padded with zeros.
    Ascii,
    /// `A`: a text's low bytes, padded with spaces.
    AsciiSpace,
    /// `b`: a bit run, low bit first.
    BitLow,
    /// `B`: a bit run, high bit first.
    BitHigh,
    /// `c`: a signed one-byte number.
    Char,
    /// `C`: an unsigned one-byte number.
    UChar,
    /// `d`: a double.
    Double,
    /// `f`: a single.
    Float,
    /// `h`: a hex run, low nibble first.
    HexLow,
    /// `H`: a hex run, high nibble first.
    HexHigh,
    /// `i` and `l`: a signed four-byte number.
    Int,
    /// `I` and `L`: an unsigned four-byte number.
    UInt,
    /// `n`: an unsigned two-byte number, network order.
    NetShort,
    /// `N`: an unsigned four-byte number, network order.
    NetLong,
    /// `p`: a pointer-like value, which writes nothing.
    Pointer,
    /// `P`: a fixed pointer-like value, which writes nothing.
    PointerFixed,
    /// `s`: a signed two-byte number.
    Short,
    /// `S`: an unsigned two-byte number.
    UShort,
    /// `v`: an unsigned two-byte number, little endian.
    LeShort,
    /// `V`: an unsigned four-byte number, little endian.
    LeLong,
    /// `u`: a uuencoded value, which is not served.
    Uuencode,
    /// `w`: a BER-compressed value, which is not served.
    Ber,
    /// `x`: zero bytes.
    Null,
    /// `X`: backs the output up.
    BackUp,
    /// `@`: pads the output to an absolute length.
    Fill,
    /// `m`: base64.
    Base64,
}

/// How many times a field applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Length {
    /// A count.
    Fixed(usize),
    /// Every value that is left.
    Tail,
}

/// One field of a format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Field {
    /// The kind.
    code: Code,
    /// The count.
    length: Length,
}

/// Packs `values` with `format`.
///
/// Answers the bytes, or `None` when the format performed no write at all: the
/// array member answers the null object for that result.
pub fn pack(format: &TjsString, values: &[Variant]) -> Result<Option<Octet>, PackError> {
    let fields = parse_template(format)?;
    let mut out = Vec::new();
    let mut value_index = 0usize;
    let mut field_index = 0usize;
    while field_index < fields.len() && value_index < values.len() {
        let field = fields[field_index];
        match field.code {
            Code::Null => {
                let count = closed_count(field.length)?;
                reserve(&mut out, count)?;
                out.resize(out.len().saturating_add(count), 0);
            }
            Code::BackUp => {
                let count = closed_count(field.length)?;
                if out.len() < count {
                    return Err(PackError::OutputUnderflow);
                }
                out.truncate(out.len() - count);
            }
            Code::Fill => {
                let count = closed_count(field.length)?;
                if count > out.len() {
                    let missing = count - out.len();
                    reserve(&mut out, missing)?;
                    out.resize(count, 0);
                }
            }
            Code::Pointer | Code::PointerFixed => {
                value_index += 1;
            }
            Code::Uuencode | Code::Ber => return Err(PackError::Unsupported),
            Code::Ascii | Code::AsciiSpace => {
                let fill = if field.code == Code::AsciiSpace {
                    b' '
                } else {
                    0
                };
                let text = values[value_index].to_string()?;
                push_ascii(&mut out, &text, fill, field.length);
                value_index += 1;
            }
            Code::BitLow | Code::BitHigh => {
                let text = values[value_index].to_string()?;
                push_bits(&mut out, &text, field.length, field.code == Code::BitHigh)?;
                value_index += 1;
            }
            Code::HexLow | Code::HexHigh => {
                let text = values[value_index].to_string()?;
                push_hex(&mut out, &text, field.length, field.code == Code::HexHigh)?;
                value_index += 1;
            }
            Code::Base64 => {
                let text = values[value_index].to_string()?;
                push_base64_decoded(&mut out, &text)?;
                value_index += 1;
            }
            Code::Double => {
                let count = numbers(field.length, values.len() - value_index);
                for _ in 0..count {
                    let value = values[value_index].to_real()?;
                    out.extend_from_slice(&value.to_bits().to_le_bytes());
                    value_index += 1;
                }
            }
            Code::Float => {
                let count = numbers(field.length, values.len() - value_index);
                for _ in 0..count {
                    let value = narrow_f32(values[value_index].to_real()?)?;
                    out.extend_from_slice(&value.to_bits().to_le_bytes());
                    value_index += 1;
                }
            }
            _ => {
                let count = numbers(field.length, values.len() - value_index);
                for _ in 0..count {
                    let value = values[value_index].to_integer()?;
                    push_integer(&mut out, value, field.code);
                    value_index += 1;
                }
            }
        }
        if field.length != Length::Tail {
            field_index += 1;
        }
    }
    if out.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Octet::from_vec(out)))
    }
}

/// Unpacks `data` with `format`.
///
/// Every field the data reaches contributes the values it holds; a field the
/// data ends before is not reached, and a numeric field whose last value is
/// cut short contributes the bytes that were there.
pub fn unpack(format: &TjsString, data: &Octet) -> Result<Vec<Variant>, PackError> {
    let fields = parse_template(format)?;
    let bytes = data.as_slice();
    let length = bytes.len();
    let mut values = Vec::new();
    let mut current = 0usize;
    let mut field_index = 0usize;
    while field_index < fields.len() && current < length {
        let field = fields[field_index];
        match field.code {
            Code::Ascii | Code::AsciiSpace => {
                let count = open_count(field.length, length - current);
                let end = current.saturating_add(count).min(length);
                let text = decode_ascii(&bytes[current..end])?;
                values.push(Variant::from(text));
                current = current.saturating_add(count);
            }
            Code::BitLow | Code::BitHigh => {
                let count = match field.length {
                    Length::Fixed(count) => count,
                    Length::Tail => (length - current).saturating_mul(8),
                };
                let text = decode_bits(bytes, current, length, count, field.code == Code::BitHigh);
                values.push(Variant::from(text));
                current = current.saturating_add(count.saturating_add(7) / 8);
            }
            Code::HexLow | Code::HexHigh => {
                let count = match field.length {
                    Length::Fixed(count) => count,
                    Length::Tail => (length - current).saturating_mul(2),
                };
                let text = decode_hex(bytes, current, length, count, field.code == Code::HexHigh);
                values.push(Variant::from(text));
                current = current.saturating_add(count.saturating_add(1) / 2);
            }
            Code::Char
            | Code::UChar
            | Code::Int
            | Code::UInt
            | Code::Short
            | Code::UShort
            | Code::NetShort
            | Code::NetLong
            | Code::LeShort
            | Code::LeLong => {
                let width = integer_width(field.code);
                let (count, advance) = match field.length {
                    Length::Fixed(count) => (
                        count,
                        count
                            .checked_mul(width)
                            .ok_or(PackError::LengthOutOfRange)?,
                    ),
                    Length::Tail => {
                        let count = (length - current) / width;
                        (count, count.saturating_mul(width))
                    }
                };
                let start = current;
                for _ in 0..count {
                    if current >= length {
                        break;
                    }
                    let take = (length - current).min(width);
                    values.push(decode_integer(
                        &bytes[current..current + take],
                        width,
                        field.code,
                    ));
                    current += take;
                }
                current = start.saturating_add(advance);
            }
            Code::Double | Code::Float => {
                let width = if field.code == Code::Double { 8 } else { 4 };
                let (count, advance) = match field.length {
                    Length::Fixed(count) => (
                        count,
                        count
                            .checked_mul(width)
                            .ok_or(PackError::LengthOutOfRange)?,
                    ),
                    Length::Tail => {
                        let count = (length - current) / width;
                        (count, count.saturating_mul(width))
                    }
                };
                let start = current;
                for _ in 0..count {
                    if current >= length {
                        break;
                    }
                    let take = (length - current).min(width);
                    values.push(decode_real(&bytes[current..current + take], width));
                    current += take;
                }
                current = start.saturating_add(advance);
            }
            Code::Null => {
                let count = open_count(field.length, length - current);
                current = current.saturating_add(count);
            }
            Code::BackUp => {
                let count = match field.length {
                    Length::Fixed(count) => count,
                    Length::Tail => current,
                };
                current -= count.min(current);
            }
            Code::Fill => {
                current = match field.length {
                    Length::Fixed(count) => count,
                    Length::Tail => length - current,
                };
            }
            Code::Base64 => {
                let text = encode_base64(&bytes[current..]);
                values.push(Variant::from(text));
                current = length;
            }
            Code::Pointer | Code::PointerFixed | Code::Uuencode | Code::Ber => {
                return Err(PackError::Unsupported);
            }
        }
        field_index += 1;
    }
    Ok(values)
}

/// The fields of a format.
fn parse_template(format: &TjsString) -> Result<Vec<Field>, PackError> {
    let units = format.as_utf16();
    let mut fields = Vec::new();
    let mut at = 0usize;
    while at < units.len() {
        let code = code_of(units[at]).ok_or(PackError::UnknownTemplate)?;
        at += 1;
        let length = if at < units.len() && units[at] == u16::from(b'*') {
            at += 1;
            Length::Tail
        } else if at < units.len() && is_digit(units[at]) {
            let mut count = 0usize;
            while at < units.len() && is_digit(units[at]) {
                let digit = usize::from(units[at] - u16::from(b'0'));
                count = count
                    .checked_mul(10)
                    .and_then(|count| count.checked_add(digit))
                    .ok_or(PackError::LengthOutOfRange)?;
                at += 1;
            }
            Length::Fixed(count)
        } else {
            Length::Fixed(1)
        };
        fields.push(Field { code, length });
    }
    Ok(fields)
}

/// The kind a format character names.
fn code_of(unit: u16) -> Option<Code> {
    let code = match unit {
        unit if unit == u16::from(b'a') => Code::Ascii,
        unit if unit == u16::from(b'A') => Code::AsciiSpace,
        unit if unit == u16::from(b'b') => Code::BitLow,
        unit if unit == u16::from(b'B') => Code::BitHigh,
        unit if unit == u16::from(b'c') => Code::Char,
        unit if unit == u16::from(b'C') => Code::UChar,
        unit if unit == u16::from(b'd') => Code::Double,
        unit if unit == u16::from(b'f') => Code::Float,
        unit if unit == u16::from(b'h') => Code::HexLow,
        unit if unit == u16::from(b'H') => Code::HexHigh,
        unit if unit == u16::from(b'i') || unit == u16::from(b'l') => Code::Int,
        unit if unit == u16::from(b'I') || unit == u16::from(b'L') => Code::UInt,
        unit if unit == u16::from(b'n') => Code::NetShort,
        unit if unit == u16::from(b'N') => Code::NetLong,
        unit if unit == u16::from(b'p') => Code::Pointer,
        unit if unit == u16::from(b'P') => Code::PointerFixed,
        unit if unit == u16::from(b's') => Code::Short,
        unit if unit == u16::from(b'S') => Code::UShort,
        unit if unit == u16::from(b'v') => Code::LeShort,
        unit if unit == u16::from(b'V') => Code::LeLong,
        unit if unit == u16::from(b'u') => Code::Uuencode,
        unit if unit == u16::from(b'w') => Code::Ber,
        unit if unit == u16::from(b'x') => Code::Null,
        unit if unit == u16::from(b'X') => Code::BackUp,
        unit if unit == u16::from(b'@') => Code::Fill,
        unit if unit == u16::from(b'm') => Code::Base64,
        _ => return None,
    };
    Some(code)
}

/// Whether the unit is an ASCII digit.
fn is_digit(unit: u16) -> bool {
    unit >= u16::from(b'0') && unit <= u16::from(b'9')
}

/// A field that consumes no value may not be unbounded.
fn closed_count(length: Length) -> Result<usize, PackError> {
    match length {
        Length::Fixed(count) => Ok(count),
        Length::Tail => Err(PackError::NoProgress),
    }
}

/// The number of values a value-consuming field takes.
fn numbers(length: Length, remaining: usize) -> usize {
    match length {
        Length::Fixed(count) => count.min(remaining),
        Length::Tail => remaining,
    }
}

/// The number of bytes a byte-consuming field takes.
fn open_count(length: Length, remaining: usize) -> usize {
    match length {
        Length::Fixed(count) => count,
        Length::Tail => remaining,
    }
}

/// Reserves room for `count` bytes, answering a typed failure.
fn reserve(out: &mut Vec<u8>, count: usize) -> Result<(), PackError> {
    out.try_reserve(count)
        .map_err(|_| PackError::LengthOutOfRange)
}

/// Packs an ASCII run.
fn push_ascii(out: &mut Vec<u8>, text: &TjsString, fill: u8, length: Length) {
    let units = text.as_utf16();
    let count = match length {
        Length::Fixed(count) => count,
        Length::Tail => units.len(),
    };
    for index in 0..count {
        match units.get(index) {
            Some(unit) => out.push(unit.to_le_bytes()[0]),
            None => out.push(fill),
        }
    }
}

/// Packs a bit run.
fn push_bits(
    out: &mut Vec<u8>,
    text: &TjsString,
    length: Length,
    high_first: bool,
) -> Result<(), PackError> {
    let count = match length {
        Length::Fixed(count) => count,
        Length::Tail => text.len(),
    };
    let mut value = 0u8;
    let mut position = if high_first { 7u32 } else { 0u32 };
    for unit in text.as_utf16().iter().take(count) {
        match *unit {
            unit if unit == u16::from(b'0') => {}
            unit if unit == u16::from(b'1') => value |= 1 << position,
            _ => return Err(PackError::BitString),
        }
        if high_first {
            if position == 0 {
                out.push(value);
                position = 7;
                value = 0;
            } else {
                position -= 1;
            }
        } else if position == 7 {
            out.push(value);
            position = 0;
            value = 0;
        } else {
            position += 1;
        }
    }
    if (high_first && position < 7) || (!high_first && position != 0) {
        out.push(value);
    }
    Ok(())
}

/// Packs a hex run.
fn push_hex(
    out: &mut Vec<u8>,
    text: &TjsString,
    length: Length,
    high_first: bool,
) -> Result<(), PackError> {
    let count = match length {
        Length::Fixed(count) => count,
        Length::Tail => text.len(),
    };
    let mut value = 0u8;
    let mut position = if high_first { 1u32 } else { 0u32 };
    for unit in text.as_utf16().iter().take(count) {
        let digit = hex_digit(*unit)?;
        value |= digit << (position * 4);
        if high_first {
            if position == 0 {
                out.push(value);
                position = 1;
                value = 0;
            } else {
                position -= 1;
            }
        } else if position != 0 {
            out.push(value);
            position = 0;
            value = 0;
        } else {
            position += 1;
        }
    }
    if (high_first && position < 1) || (!high_first && position != 0) {
        out.push(value);
    }
    Ok(())
}

/// One hex digit.
///
/// The accepted upper-case run is `A` through `E`; an upper-case `F` is not a
/// digit here.
fn hex_digit(unit: u16) -> Result<u8, PackError> {
    let digit = match unit {
        unit if unit >= u16::from(b'0') && unit <= u16::from(b'9') => {
            u8::try_from(unit - u16::from(b'0')).map_err(|_| PackError::HexString)?
        }
        unit if unit >= u16::from(b'a') && unit <= u16::from(b'f') => {
            10 + u8::try_from(unit - u16::from(b'a')).map_err(|_| PackError::HexString)?
        }
        unit if unit >= u16::from(b'A') && unit <= u16::from(b'E') => {
            10 + u8::try_from(unit - u16::from(b'A')).map_err(|_| PackError::HexString)?
        }
        _ => return Err(PackError::HexString),
    };
    Ok(digit)
}

/// Unpacks a hex run into its upper-case digits.
fn decode_hex(
    bytes: &[u8],
    start: usize,
    length: usize,
    count: usize,
    high_first: bool,
) -> TjsString {
    if count == 0 || start >= length {
        return TjsString::new();
    }
    let mut digits = Vec::new();
    for &byte in &bytes[start..length] {
        if high_first {
            push_digit(&mut digits, byte >> 4);
            if digits.len() >= count {
                break;
            }
            push_digit(&mut digits, byte & 0x0f);
        } else {
            push_digit(&mut digits, byte & 0x0f);
            if digits.len() >= count {
                break;
            }
            push_digit(&mut digits, byte >> 4);
        }
        if digits.len() >= count {
            break;
        }
    }
    digits.truncate(count);
    TjsString::from(digits)
}

/// One upper-case hex digit as a code unit.
fn push_digit(digits: &mut Vec<u16>, value: u8) {
    let character = if value < 10 {
        u16::from(b'0') + u16::from(value)
    } else {
        u16::from(b'A') + u16::from(value) - 10
    };
    digits.push(character);
}

/// Unpacks a bit run into its digits.
fn decode_bits(
    bytes: &[u8],
    start: usize,
    length: usize,
    count: usize,
    high_first: bool,
) -> TjsString {
    let mut digits = Vec::new();
    if start >= length {
        return TjsString::new();
    }
    'bytes: for &byte in &bytes[start..length] {
        for index in 0..8u32 {
            if digits.len() >= count {
                break 'bytes;
            }
            let bit = if high_first {
                (byte >> (7 - index)) & 1
            } else {
                (byte >> index) & 1
            };
            digits.push(if bit == 1 {
                u16::from(b'1')
            } else {
                u16::from(b'0')
            });
        }
    }
    digits.truncate(count);
    TjsString::from(digits)
}

/// Decodes the bytes of an ASCII run.
///
/// Zero bytes are dropped first, and the rest is read as the UTF-8 subset the
/// text encoding accepts: one byte, a two-byte run from `C2`, or a three-byte
/// run from `E0` that is not overlong. A four-byte lead and a byte that does
/// not continue its run are a typed error.
fn decode_ascii(bytes: &[u8]) -> Result<TjsString, PackError> {
    let filtered: Vec<u8> = bytes.iter().copied().filter(|byte| *byte != 0).collect();
    let mut units = Vec::new();
    let mut at = 0usize;
    while at < filtered.len() {
        let first = filtered[at];
        if first < 0x80 {
            units.push(u16::from(first));
            at += 1;
        } else if first < 0xc2 {
            return Err(PackError::Text);
        } else if first < 0xe0 {
            if at + 1 >= filtered.len() || !continues(filtered[at + 1]) {
                return Err(PackError::Text);
            }
            let unit = (u16::from(first & 0x1f) << 6) | u16::from(filtered[at + 1] & 0x3f);
            units.push(unit);
            at += 2;
        } else if first < 0xf0 {
            if at + 2 >= filtered.len()
                || !continues(filtered[at + 1])
                || !continues(filtered[at + 2])
                || (first == 0xe0 && filtered[at + 1] < 0xa0)
            {
                return Err(PackError::Text);
            }
            let unit = (u16::from(first & 0x0f) << 12)
                | (u16::from(filtered[at + 1] & 0x3f) << 6)
                | u16::from(filtered[at + 2] & 0x3f);
            units.push(unit);
            at += 3;
        } else {
            return Err(PackError::Text);
        }
    }
    Ok(TjsString::from(units))
}

/// Whether a byte continues a UTF-8 run.
fn continues(byte: u8) -> bool {
    byte & 0xc0 == 0x80
}

/// Packs one number's low bytes.
fn push_integer(out: &mut Vec<u8>, value: i64, code: Code) {
    let (width, big_endian) = match code {
        Code::Char | Code::UChar => (1, false),
        Code::Short | Code::UShort | Code::LeShort => (2, false),
        Code::NetShort => (2, true),
        Code::Int | Code::UInt | Code::LeLong => (4, false),
        Code::NetLong => (4, true),
        _ => (4, false),
    };
    let bytes = value.to_le_bytes();
    if big_endian {
        for index in (0..width).rev() {
            out.push(bytes[index]);
        }
    } else {
        out.extend_from_slice(&bytes[..width]);
    }
}

/// The number of bytes one value of the kind takes.
fn integer_width(code: Code) -> usize {
    match code {
        Code::Char | Code::UChar => 1,
        Code::Short | Code::UShort | Code::NetShort | Code::LeShort => 2,
        _ => 4,
    }
}

/// Whether the kind's value is signed.
fn is_signed(code: Code) -> bool {
    matches!(code, Code::Char | Code::Short | Code::Int)
}

/// Unpacks one number from the bytes that are there.
fn decode_integer(bytes: &[u8], width: usize, code: Code) -> Variant {
    let mut buffer = [0u8; 8];
    let big_endian = matches!(code, Code::NetShort | Code::NetLong);
    if big_endian {
        let start = width - bytes.len();
        buffer[start..width].copy_from_slice(bytes);
    } else {
        buffer[..bytes.len()].copy_from_slice(bytes);
    }
    let signed = is_signed(code);
    let value = match width {
        1 => {
            if signed {
                i64::from(i8::from_le_bytes([buffer[0]]))
            } else {
                i64::from(buffer[0])
            }
        }
        2 => {
            let bytes = [buffer[0], buffer[1]];
            if signed {
                if big_endian {
                    i64::from(i16::from_be_bytes(bytes))
                } else {
                    i64::from(i16::from_le_bytes(bytes))
                }
            } else if big_endian {
                i64::from(u16::from_be_bytes(bytes))
            } else {
                i64::from(u16::from_le_bytes(bytes))
            }
        }
        _ => {
            let bytes = [buffer[0], buffer[1], buffer[2], buffer[3]];
            if signed {
                if big_endian {
                    i64::from(i32::from_be_bytes(bytes))
                } else {
                    i64::from(i32::from_le_bytes(bytes))
                }
            } else if big_endian {
                i64::from(u32::from_be_bytes(bytes))
            } else {
                i64::from(u32::from_le_bytes(bytes))
            }
        }
    };
    Variant::from(value)
}

/// Unpacks one real from the bytes that are there.
fn decode_real(bytes: &[u8], width: usize) -> Variant {
    let mut buffer = [0u8; 8];
    buffer[..bytes.len()].copy_from_slice(bytes);
    if width == 8 {
        Variant::from(f64::from_bits(u64::from_le_bytes(buffer)))
    } else {
        let bits = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
        Variant::from(f64::from(f32::from_bits(bits)))
    }
}

/// Narrows a real to a single, the way the hardware conversion does.
///
/// The sign is kept, a value beyond the single range becomes an infinity of
/// its sign, and the significand is rounded to nearest with ties to even. A
/// not-a-number answers the quiet not-a-number of its sign.
fn narrow_f32(value: f64) -> Result<f32, PackError> {
    let bits = value.to_bits();
    let sign = bits >> 63 != 0;
    let sign_bit: u32 = if sign { 0x8000_0000 } else { 0 };
    let exponent = (bits >> 52) & 0x7ff;
    let mantissa = bits & 0x000f_ffff_ffff_ffff;
    if exponent == 0x7ff {
        return Ok(if mantissa != 0 {
            f32::from_bits(sign_bit | 0x7fc0_0000)
        } else {
            f32::from_bits(sign_bit | 0x7f80_0000)
        });
    }
    if exponent == 0 {
        return Ok(f32::from_bits(sign_bit));
    }
    let unbiased = i64::try_from(exponent).map_err(|_| PackError::LengthOutOfRange)? - 1023;
    if unbiased > 127 {
        return Ok(f32::from_bits(sign_bit | 0x7f80_0000));
    }
    let significand = mantissa | 0x0010_0000_0000_0000;
    if unbiased >= -126 {
        let mut exponent32 = unbiased;
        let mut fraction = round_shift(significand, 29);
        if fraction == 1 << 24 {
            fraction >>= 1;
            exponent32 += 1;
        }
        if exponent32 > 127 {
            return Ok(f32::from_bits(sign_bit | 0x7f80_0000));
        }
        let exponent_bits =
            u32::try_from(exponent32 + 127).map_err(|_| PackError::LengthOutOfRange)? << 23;
        let fraction_bits =
            u32::try_from(fraction & 0x007f_ffff).map_err(|_| PackError::LengthOutOfRange)?;
        Ok(f32::from_bits(sign_bit | exponent_bits | fraction_bits))
    } else {
        let shift = -unbiased - 97;
        if shift > 53 {
            return Ok(f32::from_bits(sign_bit));
        }
        let shifted = round_shift(
            significand,
            u32::try_from(shift).map_err(|_| PackError::LengthOutOfRange)?,
        );
        let bits = if shifted >= 1 << 23 {
            0x0080_0000
        } else {
            u32::try_from(shifted).map_err(|_| PackError::LengthOutOfRange)?
        };
        Ok(f32::from_bits(sign_bit | bits))
    }
}

/// Rounds `value` shifted right by `shift` to nearest, ties to even.
fn round_shift(value: u64, shift: u32) -> u64 {
    let quotient = value >> shift;
    let remainder = value & ((1u64 << shift) - 1);
    let halfway = 1u64 << (shift - 1);
    if remainder > halfway || (remainder == halfway && quotient & 1 == 1) {
        quotient + 1
    } else {
        quotient
    }
}

/// Packs a base64 text's bytes.
fn push_base64_decoded(out: &mut Vec<u8>, text: &TjsString) -> Result<(), PackError> {
    let units = text.as_utf16();
    if units.len() < 4 {
        return Ok(());
    }
    if !units.len().is_multiple_of(4) {
        return Err(PackError::Base64);
    }
    let mut at = 0usize;
    while at + 4 < units.len() {
        let first = base64_digit(units[at])?;
        let second = base64_digit(units[at + 1])?;
        let third = base64_digit(units[at + 2])?;
        let fourth = base64_digit(units[at + 3])?;
        out.push((first << 2) | (second >> 4));
        out.push((second << 4) | (third >> 2));
        out.push((third << 6) | fourth);
        at += 4;
    }
    let last = &units[units.len() - 4..];
    let first = base64_digit(last[0])?;
    let second = base64_digit(last[1])?;
    out.push((first << 2) | (second >> 4));
    let mut value = second << 4;
    if last[2] != u16::from(b'=') {
        value |= base64_digit(last[2])? >> 2;
        out.push(value);
        value = base64_digit(last[2])? << 6;
        if last[3] != u16::from(b'=') {
            value |= base64_digit(last[3])?;
            out.push(value);
        }
    }
    Ok(())
}

/// One base64 digit.
fn base64_digit(unit: u16) -> Result<u8, PackError> {
    let digit = match unit {
        unit if unit >= u16::from(b'A') && unit <= u16::from(b'Z') => {
            u8::try_from(unit - u16::from(b'A')).map_err(|_| PackError::Base64)?
        }
        unit if unit >= u16::from(b'a') && unit <= u16::from(b'z') => {
            26 + u8::try_from(unit - u16::from(b'a')).map_err(|_| PackError::Base64)?
        }
        unit if unit >= u16::from(b'0') && unit <= u16::from(b'9') => {
            52 + u8::try_from(unit - u16::from(b'0')).map_err(|_| PackError::Base64)?
        }
        unit if unit == u16::from(b'+') => 62,
        unit if unit == u16::from(b'/') => 63,
        unit if unit == u16::from(b'=') => 0,
        unit if unit < 0x0100 => 0,
        _ => return Err(PackError::Base64),
    };
    Ok(digit)
}

/// The base64 text of a byte slice.
fn encode_base64(bytes: &[u8]) -> TjsString {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    let mut at = 0usize;
    while at + 3 <= bytes.len() {
        let first = bytes[at];
        let second = bytes[at + 1];
        let third = bytes[at + 2];
        text.push(char::from(ALPHABET[usize::from(first >> 2)]));
        text.push(char::from(
            ALPHABET[usize::from(((first << 4) & 0x30) | (second >> 4))],
        ));
        text.push(char::from(
            ALPHABET[usize::from(((second << 2) & 0x3c) | (third >> 6))],
        ));
        text.push(char::from(ALPHABET[usize::from(third & 0x3f)]));
        at += 3;
    }
    match bytes.len().saturating_sub(at) {
        2 => {
            let first = bytes[at];
            let second = bytes[at + 1];
            text.push(char::from(ALPHABET[usize::from(first >> 2)]));
            text.push(char::from(
                ALPHABET[usize::from(((first << 4) & 0x30) | (second >> 4))],
            ));
            text.push(char::from(ALPHABET[usize::from((second << 2) & 0x3c)]));
            text.push('=');
        }
        1 => {
            let first = bytes[at];
            text.push(char::from(ALPHABET[usize::from(first >> 2)]));
            text.push(char::from(ALPHABET[usize::from((first << 4) & 0x30)]));
            text.push('=');
            text.push('=');
        }
        _ => {}
    }
    TjsString::from(text)
}

#[cfg(test)]
mod tests {
    use super::{PackError, pack, unpack};
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::Variant;

    /// The bytes a pack answers, which must not be the null result.
    fn bytes(values: &[Variant], format: &str) -> Vec<u8> {
        pack(&TjsString::from(format), values)
            .expect("the values pack")
            .expect("the result is an octet")
            .as_slice()
            .to_vec()
    }

    /// The values an unpack answers.
    fn unpacked(data: &[u8], format: &str) -> Vec<Variant> {
        unpack(
            &TjsString::from(format),
            &crate::vm::runtime::octet::Octet::from_slice(data),
        )
        .expect("the bytes unpack")
    }

    /// The integer a variant holds.
    fn integer(value: &Variant) -> i64 {
        value.to_integer().expect("the value is an integer")
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:12-110
    #[test]
    fn a_format_is_a_letter_and_a_count() {
        assert_eq!(
            bytes(
                &[
                    Variant::from(1i64),
                    Variant::from(2i64),
                    Variant::from(3i64)
                ],
                "i3"
            )
            .len(),
            12
        );
        assert_eq!(bytes(&[Variant::from(1i64)], "i*").len(), 4);
        assert_eq!(bytes(&[Variant::from(1i64)], "i0i")[0], 1);
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:238-285
    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:379-420
    #[test]
    fn the_number_kinds_keep_the_low_bytes() {
        assert_eq!(
            bytes(&[Variant::from(-2i64)], "i"),
            vec![0xfe, 0xff, 0xff, 0xff]
        );
        assert_eq!(bytes(&[Variant::from(-1i64)], "c"), vec![0xff]);
        assert_eq!(bytes(&[Variant::from(258i64)], "n"), vec![0x01, 0x02]);
        assert_eq!(bytes(&[Variant::from(258i64)], "v"), vec![0x02, 0x01]);
        assert_eq!(
            bytes(&[Variant::from(0x0102_0304i64)], "N"),
            vec![0x01, 0x02, 0x03, 0x04]
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:114-125
    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:526-541
    #[test]
    fn an_ascii_run_packs_low_bytes_and_reads_utf8() {
        assert_eq!(
            bytes(&[Variant::from("ab")], "a5"),
            vec![b'a', b'b', 0, 0, 0]
        );
        assert_eq!(
            bytes(&[Variant::from("ab")], "A5"),
            vec![b'a', b'b', b' ', b' ', b' ']
        );
        let packed: Vec<u8> = unpack(
            &TjsString::from("a2"),
            &crate::vm::runtime::octet::Octet::from_slice(&[0x42, 0]),
        )
        .expect("the bytes unpack")
        .into_iter()
        .map(|value| value.to_string().expect("a text").as_utf16()[0].to_le_bytes()[0])
        .collect();
        assert_eq!(packed, vec![0x42]);
        assert!(matches!(
            unpack(
                &TjsString::from("a1"),
                &crate::vm::runtime::octet::Octet::from_slice(&[0xff])
            ),
            Err(PackError::Text)
        ));
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:181-236
    #[test]
    fn an_upper_case_f_is_not_a_hex_digit() {
        assert_eq!(bytes(&[Variant::from("a1b2")], "h4"), vec![0x1a, 0x2b]);
        assert_eq!(bytes(&[Variant::from("a1b2")], "H4"), vec![0xa1, 0xb2]);
        assert!(matches!(
            pack(&TjsString::from("h2"), &[Variant::from("F0")]),
            Err(PackError::HexString)
        ));
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:286-377
    #[test]
    fn base64_reads_and_writes_whole_groups() {
        assert_eq!(bytes(&[Variant::from("SGVsbG8=")], "m"), b"Hello".to_vec());
        let values = unpacked(b"Hello", "m");
        assert_eq!(
            values[0].to_string().expect("a text"),
            TjsString::from("SGVsbG8=")
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:582-660
    #[test]
    fn a_short_number_keeps_the_bytes_that_are_there() {
        let partial = unpacked(&[1, 2, 3], "i");
        assert_eq!(partial.len(), 1);
        assert_eq!(integer(&partial[0]), 0x0003_0201);
        let clamped = unpacked(&[1, 2], "C5");
        assert_eq!(clamped.len(), 2);
        assert_eq!(integer(&clamped[1]), 2);
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:379-420
    #[test]
    fn a_field_the_values_never_reach_is_not_performed() {
        assert_eq!(
            bytes(&[Variant::from(1i64)], "ix"),
            bytes(&[Variant::from(1i64)], "i")
        );
        assert_eq!(
            pack(&TjsString::from("i"), &[]).expect("the empty pack"),
            None
        );
    }

    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:257-285
    // reference/krkr2/cpp/core/tjs2/tjsOctPack.cpp:446-474
    #[test]
    fn a_single_is_rounded_to_the_single_range() {
        assert_eq!(
            bytes(&[Variant::from(1.5f64)], "f"),
            vec![0x00, 0x00, 0xc0, 0x3f]
        );
        assert_eq!(
            bytes(&[Variant::from(1e40f64)], "f"),
            vec![0x00, 0x00, 0x80, 0x7f]
        );
        assert_eq!(
            bytes(&[Variant::from(-0.0f64)], "f"),
            vec![0x00, 0x00, 0x00, 0x80]
        );
    }
}
