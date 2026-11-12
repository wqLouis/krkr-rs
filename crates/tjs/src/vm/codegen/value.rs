//! The values the generator folds with.
//!
//! The generator folds constant expressions: `1 + 2` becomes one constant.
//! The folding follows the variant's own semantics, so the values here
//! carry the same distinctions the variant carries.
//!
//! An **object** is held **by identity**: the generator never looks inside
//! one. The identity is the address of the object's dispatch, which is
//! what the generator prints and what the generator compares. An object
//! with no dispatch — the null — has the identity zero.

use std::fmt;
use std::num::IntErrorKind;

/// The identity of an object.
///
/// The identity is the address of the object's dispatch. The null object
/// has no dispatch, so its identity is zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub u64);

impl ObjectId {
    /// The identity of an object with no dispatch.
    pub const NULL: ObjectId = ObjectId(0);

    /// Whether the object is the one with no dispatch.
    pub fn is_null(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:x}", self.0)
    }
}

/// The value of a constant expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The value of no value.
    Void,
    /// An integer.
    Integer(i64),
    /// A real number.
    Real(f64),
    /// A string.
    String(String),
    /// A byte string.
    Octet(Vec<u8>),
    /// An object, held by its identity.
    Object(ObjectId),
    /// A context of the block, held by its index.
    ///
    /// `RegisterFunction` puts the child context object into the parent's
    /// constant area. The generator has no object to put there, so it
    /// names the context by its index in the block, and the side that has
    /// the object resolves the index.
    Context(usize),
    /// A constant array: the array the `(const)[...]` form builds, and the
    /// elements it puts in it.
    ///
    /// The array is built at compile time and the elements are added
    /// through the array's own `add`. The generator cannot build a live
    /// object, so it carries the elements and the side that has the
    /// object builds it.
    ConstArray {
        /// The identity of the array: two constant arrays are two
        /// objects, and the generator never merges them.
        identity: ObjectId,
        /// The elements, in the order the grammar added them.
        elements: Vec<Value>,
    },
    /// A constant dictionary: the dictionary the `(const)%[...]` form builds,
    /// and the entries it puts in it.
    ///
    /// The dictionary is built at compile time and the entries are set
    /// with PropSet.
    ConstDictionary {
        /// The identity of the dictionary: two constant dictionaries are
        /// two objects, and the generator never merges them.
        identity: ObjectId,
        /// The entries, in the order the grammar set them: the value the
        /// name of the member is made of, and the value it holds.
        ///
        /// The name is a value and not a text, because the side that
        /// builds the object converts it to a text itself.
        entries: Vec<(Value, Value)>,
    },
}

/// The reason a value cannot be converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueError {
    /// The value has no conversion to the wanted type.
    Convert,
}

impl fmt::Display for ValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueError::Convert => write!(formatter, "the value cannot be converted"),
        }
    }
}

impl std::error::Error for ValueError {}

/// The value as a string.
///
/// An object converts to its own text, which is printed as
/// `(object <address>:<address>)`; the null has no address, so it
/// prints as `(object 0x0:0x0)`. The conversion never fails.
impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Void => Ok(()),
            Value::Integer(value) => write!(formatter, "{value}"),
            Value::Real(value) => formatter.write_str(&format_real(*value)),
            Value::String(value) => formatter.write_str(value),
            Value::Octet(bytes) => {
                let text: String = bytes.iter().map(|byte| *byte as char).collect();
                formatter.write_str(&text)
            }
            Value::Object(id) => formatter.write_str(&object_text(*id)),
            Value::Context(index) => formatter.write_str(&context_text(*index)),
            Value::ConstArray { identity, .. } => formatter.write_str(&object_text(*identity)),
            Value::ConstDictionary { identity, .. } => formatter.write_str(&object_text(*identity)),
        }
    }
}

impl Value {
    /// The value as its text.
    ///
    /// `ToString` raises on a byte string, so the conversion of one fails
    /// here too.
    pub fn to_text(&self) -> Option<String> {
        match self {
            Value::Octet(_) => None,
            other => Some(other.to_string()),
        }
    }

    /// The conversion fails when the value has no numeric conversion;
    /// the object has none, so it fails too.
    pub fn to_real(&self) -> Option<f64> {
        match self {
            Value::Void => Some(0.0),
            Value::Integer(value) => Some(*value as f64),
            Value::Real(value) => Some(*value),
            Value::String(value) => Some(match parse_number(value) {
                Some(parsed) => parsed.to_real().unwrap_or(0.0),
                None => 0.0,
            }),
            Value::Octet(_)
            | Value::Object(_)
            | Value::Context(_)
            | Value::ConstArray { .. }
            | Value::ConstDictionary { .. } => None,
        }
    }

    /// The value as an integer.
    ///
    /// The conversion fails when the value has no integer conversion: an
    /// object and a byte string have none. A string that is not a number
    /// is zero, and a value of no value is zero.
    pub fn to_integer(&self) -> Option<i64> {
        match self {
            Value::Void => Some(0),
            Value::Integer(value) => Some(*value),
            Value::Real(value) => Some(*value as i64),
            Value::String(value) => Some(match parse_number(value) {
                Some(parsed) => parsed.to_integer().unwrap_or(0),
                None => 0,
            }),
            Value::Octet(_)
            | Value::Object(_)
            | Value::Context(_)
            | Value::ConstArray { .. }
            | Value::ConstDictionary { .. } => None,
        }
    }

    /// The value as a number.
    ///
    /// A string that reads as a number becomes that number — an integer
    /// when it reads as one, a real when it reads as one — and a string
    /// that reads as nothing becomes the integer zero.
    pub fn to_number(&self) -> Option<Value> {
        match self {
            Value::Void => Some(Value::Integer(0)),
            Value::Integer(_) | Value::Real(_) => Some(self.clone()),
            Value::String(value) => Some(parse_number(value).unwrap_or(Value::Integer(0))),
            Value::Octet(_)
            | Value::Object(_)
            | Value::Context(_)
            | Value::ConstArray { .. }
            | Value::ConstDictionary { .. } => None,
        }
    }

    /// The value as a real number, for the arithmetic that needs one.
    pub fn as_real(&self) -> Result<f64, ValueError> {
        self.to_real().ok_or(ValueError::Convert)
    }

    /// The value as a string, for the comparison that needs one.
    ///
    /// The comparison converts a value to a string when the two values
    /// are of different kinds and one of them is a string. An object
    /// converts to its text, so it takes this path too.
    pub fn as_string_for_compare(&self) -> Result<String, ValueError> {
        match self {
            Value::Object(id) => Ok(object_text(*id)),
            _ => Ok(self.to_string()),
        }
    }

    /// Whether the value is true.
    ///
    /// `operator bool` reads a string as its integer, so the string
    /// `"0"` is false and the string `" "` is true. An object is true
    /// when it has a dispatch; the null has none, so it is false.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Void => false,
            Value::Integer(value) => *value != 0,
            Value::Real(value) => *value != 0.0,
            Value::String(_) => self.to_integer().unwrap_or(0) != 0,
            Value::Octet(bytes) => !bytes.is_empty(),
            Value::Object(id) => !id.is_null(),
            Value::Context(_) => true,
            Value::ConstArray { .. } | Value::ConstDictionary { .. } => true,
        }
    }

    /// The comparison of two values.
    ///
    /// The conversion error is caught and reported as "the two values are
    /// not equal", so the comparison never fails.
    pub fn normal_compare(left: &Value, right: &Value) -> bool {
        Self::compare_normal(left, right).unwrap_or_default()
    }

    /// The comparison of two values, when it answers whether they are the
    /// same kind of value.
    ///
    /// `DiscernCompare` answers false when the two values are of
    /// different kinds, and compares the two values of the same kind on
    /// their own terms.
    pub fn discern_compare(left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Void, Value::Void) => true,
            (Value::Integer(left), Value::Integer(right)) => left == right,
            (Value::Real(left), Value::Real(right)) => {
                // a real that is not a number is never equal to anything,
                // and the infinities are equal to themselves
                if left.is_nan() || right.is_nan() {
                    return false;
                }
                left == right
            }
            (Value::String(left), Value::String(right)) => left == right,
            (Value::Octet(left), Value::Octet(right)) => left == right,
            (Value::Object(left), Value::Object(right)) => left == right,
            (Value::Context(left), Value::Context(right)) => left == right,
            (
                Value::ConstArray { identity: left, .. },
                Value::ConstArray {
                    identity: right, ..
                },
            ) => left == right,
            (
                Value::ConstDictionary { identity: left, .. },
                Value::ConstDictionary {
                    identity: right, ..
                },
            ) => left == right,
            _ => false,
        }
    }

    /// The comparison of two values, with the conversion error.
    fn compare_normal(left: &Value, right: &Value) -> Result<bool, ValueError> {
        use Value::*;
        match (left, right) {
            (Integer(a), Integer(b)) => Ok(a == b),
            (Real(a), Real(b)) => Ok(a == b),
            (String(a), String(b)) => Ok(a == b),
            (Octet(a), Octet(b)) => Ok(a == b),
            (Object(a), Object(b)) => Ok(a == b),
            (Context(a), Context(b)) => Ok(a == b),
            (ConstArray { identity: a, .. }, ConstArray { identity: b, .. }) => Ok(a == b),
            (ConstDictionary { identity: a, .. }, ConstDictionary { identity: b, .. }) => {
                Ok(a == b)
            }
            (Integer(a), Real(b)) => Ok(*a as f64 == *b),
            (Real(a), Integer(b)) => Ok(*a == *b as f64),
            (Void, Integer(b)) => Ok(*b == 0),
            (Integer(a), Void) => Ok(*a == 0),
            (Void, Real(b)) => Ok(*b == 0.0),
            (Real(a), Void) => Ok(*a == 0.0),
            // both are converted to a string when one of the two is a
            // string
            (String(_), _) | (_, String(_)) => {
                let a = left.as_string_for_compare()?;
                let b = right.as_string_for_compare()?;
                Ok(a == b)
            }
            _ => {
                let a = left.as_real()?;
                let b = right.as_real()?;
                Ok(a == b)
            }
        }
    }
}

/// The text of an object.
///
/// The text holds the address of the object's dispatch and the address
/// of the object's `objthis`. The generator holds the object by its
/// identity alone, so the `objthis` address is zero.
fn object_text(id: ObjectId) -> String {
    format!("(object {}:0x0)", id)
}

/// The text of a context.
///
/// A context is named by its index in the block, so its text names the
/// index.
fn context_text(index: usize) -> String {
    format!("(object context:{index}:0x0)")
}

/// The number the text holds, when the text holds a number.
///
/// `TJSParseNumber`: an optional sign, then `true`, `false`, `NaN`,
/// `Infinity`, or a number in the base its prefix names.
fn parse_number(text: &str) -> Option<Value> {
    let mut rest = text;
    let mut negative = false;
    if let Some(first) = rest.chars().next()
        && (first == '+' || first == '-')
    {
        negative = first == '-';
        rest = &rest[first.len_utf8()..];
        rest = rest.trim_start();
    }
    let value = parse_number_body(rest)?;
    Some(if negative {
        match value {
            Value::Integer(number) => Value::Integer(number.wrapping_neg()),
            Value::Real(number) => Value::Real(-number),
            other => other,
        }
    } else {
        value
    })
}

/// The number the text holds, without a sign.
fn parse_number_body(text: &str) -> Option<Value> {
    // the words that are read as numbers
    if text.len() >= 4 && text[..4].eq_ignore_ascii_case("true") {
        return Some(Value::Integer(1));
    }
    if text.len() >= 5 && text[..5].eq_ignore_ascii_case("false") {
        return Some(Value::Integer(0));
    }
    if text.len() >= 3 && text[..3].eq_ignore_ascii_case("NaN") {
        return Some(Value::Real(f64::NAN));
    }
    if text.len() >= 8 && text[..8].eq_ignore_ascii_case("Infinity") {
        return Some(Value::Real(f64::INFINITY));
    }

    // the numbers a prefix names
    for (prefix, base) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            let digits: String = rest
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect();
            return i64::from_str_radix(&digits, base).ok().map(Value::Integer);
        }
    }

    // the octal number a leading zero names
    if text.len() > 1 && text.starts_with('0') {
        let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.len() > 1 && digits.chars().all(|c| ('0'..='7').contains(&c)) {
            return i64::from_str_radix(&digits, 8).ok().map(Value::Integer);
        }
    }

    // the decimal number: the longest prefix that reads as one
    let bytes = text.as_bytes();
    let mut end = 0;
    let mut digits = false;
    let mut point = false;
    let mut exponent = false;
    while end < bytes.len() {
        let byte = bytes[end];
        if byte.is_ascii_digit() {
            digits = true;
            end += 1;
        } else if byte == b'.' && !point && !exponent {
            point = true;
            end += 1;
        } else if (byte == b'e' || byte == b'E') && digits && !exponent {
            exponent = true;
            end += 1;
            if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
                end += 1;
            }
        } else {
            break;
        }
    }
    if !digits {
        return None;
    }
    let number = &text[..end];
    if point || exponent {
        number.parse::<f64>().ok().map(Value::Real)
    } else {
        // the text is a run of digits, so the only failure an integer
        // parse can have is an overflow, and a literal too large for an
        // integer is a real
        match number.parse::<i64>() {
            Ok(value) => Some(Value::Integer(value)),
            Err(error) => match error.kind() {
                IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
                    number.parse::<f64>().ok().map(Value::Real)
                }
                _ => None,
            },
        }
    }
}

/// The text of a real number.
///
/// The text is the shortest form that reads back as the same number.
fn format_real(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Infinity".to_string()
        } else {
            "+Infinity".to_string()
        };
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "+0.0".to_string()
        };
    }
    let negative = value.is_sign_negative();
    // fifteen significant digits
    let scientific = format!("{:.*e}", 14, value.abs());
    let (mantissa, exponent) = scientific
        .split_once('e')
        .unwrap_or((scientific.as_str(), "0"));
    // the scientific form of a finite number always carries an exponent,
    // and the exponent always reads as a decimal integer
    let exponent: i32 = exponent
        .parse()
        .expect("the scientific form carries an exponent");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    if !(-4..15).contains(&exponent) {
        // the exponent form
        text.push(digits.as_bytes()[0] as char);
        let rest = digits[1..].trim_end_matches('0');
        if !rest.is_empty() {
            text.push('.');
            text.push_str(rest);
        }
        text.push('e');
        text.push(if exponent < 0 { '-' } else { '+' });
        text.push_str(&format!("{:02}", exponent.abs()));
    } else {
        // the plain form
        let point = exponent + 1;
        if point <= 0 {
            text.push_str("0.");
            for _ in 0..-point {
                text.push('0');
            }
            text.push_str(digits.trim_end_matches('0'));
        } else {
            let point = point as usize;
            if digits.len() <= point {
                text.push_str(&digits);
                for _ in digits.len()..point {
                    text.push('0');
                }
            } else {
                text.push_str(&digits[..point]);
                let rest = digits[point..].trim_end_matches('0');
                if !rest.is_empty() {
                    text.push('.');
                    text.push_str(rest);
                }
            }
        }
    }
    text
}
