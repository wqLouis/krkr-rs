//! The string a script's text values are.
//!
//! A script string is a sequence of UTF-16 code units, and its length is the
//! number of code units: a character outside the basic plane takes two of
//! them, and a lone surrogate takes one. A value is immutable and owns its
//! storage behind a shared handle, so cloning a value never copies the units.
//!
//! # The zero code unit
//!
//! A value's text ends at the first zero code unit. A script string cannot
//! carry a zero — the escape that writes one ends the text it was written in
//! — and every construction here applies that rule in one place, so the
//! length, the units, the comparison and the hash all agree.
//!
//! # Comparison
//!
//! Two values compare by code unit, in order, and the case is not folded:
//! `"A"` sorts before `"a"`, and a text that is a prefix of a longer one
//! sorts first.
//!
//! # Messages
//!
//! [`fmt::Display`] writes the text with a lone surrogate shown as the
//! replacement character, because a message is a sequence of characters and
//! cannot hold half of a pair. [`fmt::Debug`] writes such a code unit as its
//! escape, and [`TjsString::as_utf16`] hands the units back unchanged, so no
//! caller has to lose one.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

use super::number::{self, Number, NumberError};

/// A script string: an immutable sequence of UTF-16 code units.
#[derive(Clone)]
pub struct TjsString {
    /// The code units, shared between the values that hold the same text.
    units: Arc<[u16]>,
}

impl TjsString {
    /// The empty string.
    pub fn new() -> Self {
        Self::from_units(Vec::new())
    }

    /// The string of the given code units, cut at the first zero.
    ///
    /// This is the one place a value is built, so the zero rule holds for
    /// every construction.
    fn from_units(mut units: Vec<u16>) -> Self {
        if let Some(end) = units.iter().position(|&unit| unit == 0) {
            units.truncate(end);
        }
        Self {
            units: Arc::from(units),
        }
    }

    /// The number of code units.
    pub fn len(&self) -> usize {
        self.units.len()
    }

    /// Whether the value holds no code units.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// The code units.
    pub fn as_utf16(&self) -> &[u16] {
        &self.units
    }

    /// The text with the text of `other` after it.
    pub fn concat(&self, other: &Self) -> Self {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        let mut units = Vec::with_capacity(self.len() + other.len());
        units.extend_from_slice(&self.units);
        units.extend_from_slice(&other.units);
        Self::from_units(units)
    }

    /// The hash an object's member table buckets this text by.
    ///
    /// The value is the one an object's member lookup computes: a hash over
    /// the code units, and zero for the empty value.
    pub fn member_hash(&self) -> u32 {
        if self.units.is_empty() {
            return 0;
        }
        let mut hash: u32 = 0;
        for &unit in self.units.iter() {
            hash = hash.wrapping_add(u32::from(unit));
            hash = hash.wrapping_add(hash.wrapping_shl(10));
            hash ^= hash >> 6;
        }
        hash = hash.wrapping_add(hash.wrapping_shl(3));
        hash ^= hash >> 11;
        hash = hash.wrapping_add(hash.wrapping_shl(15));
        if hash == 0 { u32::MAX } else { hash }
    }

    /// The text as a script's escape rules write it.
    ///
    /// The control characters that have a named escape are written as that
    /// escape, the backslash and both quotes are escaped, and the other
    /// control code units are written in hexadecimal. A hexadecimal digit
    /// that follows a control code unit is written in hexadecimal as well,
    /// so the digit is not read as part of the escape before it.
    pub fn escape_c(&self) -> Self {
        let mut units: Vec<u16> = Vec::with_capacity(self.len());
        let mut hexadecimal = false;
        for &unit in self.units.iter() {
            if let Some(escape) = control_escape(unit) {
                units.extend(escape.encode_utf16());
                hexadecimal = false;
                continue;
            }
            if hexadecimal && is_hexadecimal_digit(unit) {
                push_hexadecimal(&mut units, unit);
                continue;
            }
            if unit < 0x20 {
                hexadecimal = true;
                push_hexadecimal(&mut units, unit);
                continue;
            }
            hexadecimal = false;
            units.push(unit);
        }
        Self::from_units(units)
    }

    /// The text an escaped value stands for.
    ///
    /// The value is empty. Escape sequences are decoded while a string
    /// literal is scanned, and no value-level call decodes them.
    pub fn unescape_c(&self) -> Self {
        Self::new()
    }

    /// The number this text is read as.
    ///
    /// A script's arithmetic reads text that is not a number as the integer
    /// zero; this call keeps that failure distinct, so the one conversion
    /// that applies the rule is the one that decides it.
    ///
    /// # Errors
    ///
    /// [`NumberError::NotANumber`] when the text does not begin with a
    /// number.
    pub fn to_number(&self) -> Result<Number, NumberError> {
        number::parse(self.as_utf16()).map(|parsed| parsed.number)
    }

    /// The integer this text is read as, with a real truncated toward zero.
    ///
    /// # Errors
    ///
    /// [`NumberError::NotANumber`] when the text does not begin with a
    /// number, and [`NumberError::RealOutOfRange`] when it is a real that no
    /// 64-bit integer can hold.
    pub fn to_integer(&self) -> Result<i64, NumberError> {
        let parsed = number::parse(self.as_utf16())?;
        parsed.number.to_integer()
    }

    /// The real this text is read as.
    ///
    /// # Errors
    ///
    /// [`NumberError::NotANumber`] when the text does not begin with a
    /// number.
    pub fn to_real(&self) -> Result<f64, NumberError> {
        let parsed = number::parse(self.as_utf16())?;
        Ok(parsed.number.to_real())
    }
}

impl Default for TjsString {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&str> for TjsString {
    fn from(text: &str) -> Self {
        Self::from_units(text.encode_utf16().collect())
    }
}

impl From<String> for TjsString {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&[u16]> for TjsString {
    fn from(units: &[u16]) -> Self {
        Self::from_units(units.to_vec())
    }
}

impl From<Vec<u16>> for TjsString {
    fn from(units: Vec<u16>) -> Self {
        Self::from_units(units)
    }
}

impl From<&Number> for TjsString {
    fn from(number: &Number) -> Self {
        Self::from(number.to_string())
    }
}

impl PartialEq for TjsString {
    fn eq(&self, other: &Self) -> bool {
        self.units == other.units
    }
}

impl Eq for TjsString {}

impl crate::vm::runtime::hash::TableKey for TjsString {
    /// The member hash the table files this text under.
    fn table_hash(&self) -> u32 {
        self.member_hash()
    }
}

impl PartialOrd for TjsString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TjsString {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_utf16().cmp(other.as_utf16())
    }
}

impl fmt::Display for TjsString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&String::from_utf16_lossy(&self.units))
    }
}

impl fmt::Debug for TjsString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("\"")?;
        for unit in char::decode_utf16(self.units.iter().copied()) {
            match unit {
                Ok(character) => {
                    for escaped in character.escape_debug() {
                        write!(formatter, "{escaped}")?;
                    }
                }
                Err(error) => {
                    let unit = error.unpaired_surrogate();
                    write!(formatter, "\\u{{{unit:04x}}}")?;
                }
            }
        }
        formatter.write_str("\"")
    }
}

/// The escape a named control code unit is written as, if it has one.
fn control_escape(unit: u16) -> Option<&'static str> {
    match unit {
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
    }
}

/// Whether the code unit is a hexadecimal digit.
fn is_hexadecimal_digit(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39 | 0x41..=0x46 | 0x61..=0x66)
}

/// Write a code unit as a two-digit hexadecimal escape.
fn push_hexadecimal(units: &mut Vec<u16>, unit: u16) {
    const DIGITS: [u8; 16] = *b"0123456789abcdef";
    units.push(u16::from(b'\\'));
    units.push(u16::from(b'x'));
    units.push(u16::from(DIGITS[usize::from((unit >> 4) & 0x0f)]));
    units.push(u16::from(DIGITS[usize::from(unit & 0x0f)]));
}

#[cfg(test)]
mod tests {
    use super::TjsString;
    use crate::vm::runtime::number::{self, Number, NumberError};

    // reference/cpp/core/tjs2/tjsVariantString.cpp:500-504
    #[test]
    fn the_length_counts_code_units() {
        let outside_the_basic_plane = TjsString::from("\u{29e3d}");
        assert_eq!(outside_the_basic_plane.len(), 2);
        assert_eq!(outside_the_basic_plane.as_utf16(), &[0xd867, 0xde3d]);
        let lone_surrogate = TjsString::from(vec![0xd800u16]);
        assert_eq!(lone_surrogate.len(), 1);
        assert_eq!(lone_surrogate.as_utf16(), &[0xd800]);
        assert!(TjsString::new().is_empty());
        assert_eq!(TjsString::from("abc").as_utf16(), &[0x61, 0x62, 0x63]);
    }

    // reference/cpp/core/tjs2/tjsVariantString.cpp:507-516
    // reference/cpp/core/tjs2/tjsLex.cpp:290-306
    #[test]
    fn a_zero_code_unit_ends_the_text() {
        let text = TjsString::from(vec![0x61u16, 0x00, 0x62]);
        assert_eq!(text.len(), 1);
        assert_eq!(text, TjsString::from("a"));
        assert!(TjsString::from("\0").is_empty());
        assert!(TjsString::from(vec![0u16]).is_empty());
    }

    // reference/cpp/core/tjs2/tjsConfig.cpp:551-566
    // reference/cpp/core/tjs2/tjsString.h:167-176
    // reference/cpp/core/tjs2/tjsString.h:211-217
    #[test]
    fn the_comparison_is_by_code_unit() {
        assert_eq!(
            TjsString::from("abc"),
            TjsString::from(vec![0x61u16, 0x62, 0x63])
        );
        assert!(TjsString::from("abc") < TjsString::from("abd"));
        assert!(TjsString::from("ab") < TjsString::from("abc"));
        // the case is not folded: `A` is 0x41 and `a` is 0x61
        assert!(TjsString::from("A") < TjsString::from("a"));
        assert_ne!(TjsString::from("ABC"), TjsString::from("abc"));
        // a code unit decides, not a code point: the pair of a character
        // outside the basic plane starts below 0xffff
        assert!(TjsString::from("\u{29e3d}") < TjsString::from("\u{ffff}"));
    }

    // reference/cpp/core/tjs2/tjsHashSearch.h:57-78
    #[test]
    fn the_member_hash_is_the_routines_value() {
        assert_eq!(TjsString::new().member_hash(), 0);
        assert_eq!(TjsString::from("a").member_hash(), 3_392_050_242);
        assert_eq!(TjsString::from("abc").member_hash(), 3_977_453_403);
        assert_eq!(TjsString::from("length").member_hash(), 2_508_072_249);
    }

    // reference/cpp/core/tjs2/tjsHashSearch.h:57-78
    // reference/cpp/core/tjs2/tjsHashSearch.h:243
    #[test]
    fn the_table_key_is_the_member_hash() {
        use crate::vm::runtime::hash::TableKey;

        let pinned = [
            (TjsString::new(), 0u32),
            (TjsString::from("a"), 3_392_050_242),
            (TjsString::from("abc"), 3_977_453_403),
            (TjsString::from("length"), 2_508_072_249),
        ];
        for (value, member_hash) in pinned {
            assert_eq!(TableKey::table_hash(&value), member_hash);
            assert_eq!(TableKey::table_hash(&value), value.member_hash());
        }
    }

    // reference/cpp/core/tjs2/tjsString.h:262-273
    #[test]
    fn concatenation_appends_the_units() {
        // 21 code units is the longest text the shipped string keeps
        // inline, and a longer one is a buffer; this value has one
        // representation at every length, so the boundary changes nothing
        let ten = TjsString::from("0123456789");
        let twenty = ten.concat(&ten);
        assert_eq!(twenty.len(), 20);
        let twenty_one = twenty.concat(&TjsString::from("0"));
        assert_eq!(twenty_one, TjsString::from("012345678901234567890"));
        assert_eq!(twenty_one.len(), 21);
        let twenty_two = twenty_one.concat(&TjsString::from("1"));
        assert_eq!(twenty_two, TjsString::from("0123456789012345678901"));
        assert_eq!(twenty_two.len(), 22);
        assert_eq!(TjsString::new().concat(&TjsString::new()), TjsString::new());
        assert_eq!(TjsString::new().concat(&ten), ten);
        assert_eq!(ten.concat(&TjsString::new()), ten);
    }

    // reference/cpp/core/tjs2/tjsString.cpp:160-230
    #[test]
    fn escaping_writes_the_named_escapes() {
        let controls = TjsString::from(vec![0x07u16, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(
            controls.escape_c(),
            TjsString::from("\\a\\b\\t\\n\\v\\f\\r")
        );
        assert_eq!(TjsString::from("\\").escape_c(), TjsString::from("\\\\"));
        assert_eq!(TjsString::from("'").escape_c(), TjsString::from("\\'"));
        assert_eq!(TjsString::from("\"").escape_c(), TjsString::from("\\\""));
        assert_eq!(
            TjsString::from("\u{01}").escape_c(),
            TjsString::from("\\x01")
        );
        // a hexadecimal digit after a control unit is written as an escape
        assert_eq!(
            TjsString::from("\u{01}a").escape_c(),
            TjsString::from("\\x01\\x61")
        );
        assert_eq!(
            TjsString::from("\u{01}z").escape_c(),
            TjsString::from("\\x01z")
        );
        assert_eq!(
            TjsString::from("\u{01}\u{02}").escape_c(),
            TjsString::from("\\x01\\x02")
        );
        assert_eq!(
            TjsString::from("plain text").escape_c(),
            TjsString::from("plain text")
        );
        // a lone surrogate is a code unit like any other
        assert_eq!(
            TjsString::from(vec![0xd800u16]).escape_c().as_utf16(),
            &[0xd800]
        );
    }

    // reference/cpp/core/tjs2/tjsString.cpp:232-235
    #[test]
    fn unescaping_answers_an_empty_value() {
        assert!(TjsString::from("\\n").unescape_c().is_empty());
        assert!(TjsString::from("abc").unescape_c().is_empty());
        assert!(TjsString::new().unescape_c().is_empty());
    }

    // reference/cpp/core/tjs2/tjsVariantString.cpp:481-493
    // reference/cpp/core/tjs2/tjsVariantString.cpp:457-478
    // reference/cpp/core/tjs2/tjsLex.cpp:700-727
    #[test]
    fn the_arithmetic_coercion_reads_the_number() {
        assert_eq!(
            TjsString::from("12abc").to_number(),
            Ok(Number::Integer(12))
        );
        assert_eq!(TjsString::from("0x1f").to_number(), Ok(Number::Integer(31)));
        assert_eq!(TjsString::from("12.5").to_number(), Ok(Number::Real(12.5)));
        assert_eq!(TjsString::from("12.5").to_integer(), Ok(12));
        assert_eq!(TjsString::from("12").to_real(), Ok(12.0));
        // a text that is not a number is its own outcome, not a zero
        assert_eq!(
            TjsString::from(" 12 ").to_number(),
            Err(NumberError::NotANumber)
        );
        assert_eq!(
            TjsString::from("").to_integer(),
            Err(NumberError::NotANumber)
        );
        // the one number entry is the one the coercion reads
        assert_eq!(
            TjsString::from("1e3").to_number(),
            number::parse(&[0x31u16, 0x65, 0x33]).map(|parsed| parsed.number)
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:213-249
    // reference/cpp/core/tjs2/tjsVariant.h:755-771
    #[test]
    fn a_number_is_written_as_text() {
        assert_eq!(TjsString::from(&Number::Integer(12)), TjsString::from("12"));
        assert_eq!(
            TjsString::from(&Number::Integer(i64::MIN)),
            TjsString::from("-9223372036854775808")
        );
        assert_eq!(TjsString::from(&Number::Real(0.5)), TjsString::from("0.5"));
        assert_eq!(
            TjsString::from(&Number::Real(-0.0)),
            TjsString::from("-0.0")
        );
        assert_eq!(
            TjsString::from(&Number::Real(f64::NAN)),
            TjsString::from("NaN")
        );
    }

    // reference/cpp/core/tjs2/tjsVariantString.h:72-81
    #[test]
    fn the_display_form_substitutes_a_lone_surrogate() {
        let text = TjsString::from(vec![0x61u16, 0xd800, 0x62]);
        assert_eq!(text.to_string(), "a\u{fffd}b");
        assert_eq!(format!("{text:?}"), "\"a\\u{d800}b\"");
        assert_eq!(text.as_utf16(), &[0x61, 0xd800, 0x62]);
    }
}
