//! The two values that are not objects, and the members a call reaches on them.
//!
//! A text and a byte string are values, not objects, and the engine registers no
//! class object for either: a call on one of them reaches its own member surface
//! before any object lookup, and a name that is not in these tables is a missing
//! member rather than an object's refusal. This module is that surface — one
//! table per value kind, each row a name, an arity and the rule behind it — so a
//! class object that publishes the same members, if one is ever registered,
//! publishes exactly these names and reads these rules.
//!
//! The table is read by the call diversion, and only by it: a *read* of one of
//! these names is not a method value at all, it is a missing member.
//!
//! # What a rule reads
//!
//! A parameter that must be a number is read as one: a text that is not a
//! number is the integer zero, and an object or an octet has no number. A
//! parameter that must be a text is read as one, so an object is its form and an
//! octet has no text. Two of the text members are shims over another object —
//! `replace` asks the pattern value and `split` asks a fresh array — and they
//! reach it through the object surface, which is where the engine reaches it.
//!
//! # The byte string
//!
//! A byte string publishes one member. Its `unpack` is the pack format's
//! reverse direction, [`unpack`](crate::vm::runtime::octet_pack::unpack), and it
//! answers the array the bytes rebuild.

use std::rc::Rc;

use crate::vm::runtime::array::{ArrayError, ArrayObject};
use crate::vm::runtime::object::Dispatch;
use crate::vm::runtime::octet::Octet;
use crate::vm::runtime::octet_pack::{self, PackError};
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Closure, Variant, VariantError};

/// A member of a text's method surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StringMember {
    /// The one unit at a position, or the empty text outside the text.
    CharAt,
    /// The position of a text inside the text.
    IndexOf,
    /// The text with the ASCII lower-case letters upper-cased.
    ToUpperCase,
    /// The text with the ASCII upper-case letters lower-cased.
    ToLowerCase,
    /// The text from a position, or from it for a count.
    Substring,
    /// The same rule as [`StringMember::Substring`] under its other name.
    Substr,
    /// The text a format string builds from the parameters.
    Sprintf,
    /// The text with a pattern's matches replaced by another text.
    Replace,
    /// The text with its control units and quotes escaped.
    Escape,
    /// The texts a pattern splits the text into, as an array.
    Split,
    /// The text with the units from one to `0x20` cut from both ends.
    Trim,
    /// The text with its units in the other order.
    Reverse,
    /// The text repeated a number of times.
    Repeat,
}

impl StringMember {
    /// Every member, in the order the engine's own name list has them.
    pub const ALL: &'static [Self] = &[
        Self::CharAt,
        Self::IndexOf,
        Self::ToUpperCase,
        Self::ToLowerCase,
        Self::Substring,
        Self::Substr,
        Self::Sprintf,
        Self::Replace,
        Self::Escape,
        Self::Split,
        Self::Trim,
        Self::Reverse,
        Self::Repeat,
    ];

    /// The member `name` denotes, or `None` when it names no member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        let units = name.as_utf16();
        TEXT_MEMBERS
            .iter()
            .find(|spec| units.iter().copied().eq(spec.name.encode_utf16()))
            .map(|spec| spec.member)
    }

    /// The name the member is published under.
    pub fn name(self) -> &'static str {
        match self {
            Self::CharAt => "charAt",
            Self::IndexOf => "indexOf",
            Self::ToUpperCase => "toUpperCase",
            Self::ToLowerCase => "toLowerCase",
            Self::Substring => "substring",
            Self::Substr => "substr",
            Self::Sprintf => "sprintf",
            Self::Replace => "replace",
            Self::Escape => "escape",
            Self::Split => "split",
            Self::Trim => "trim",
            Self::Reverse => "reverse",
            Self::Repeat => "repeat",
        }
    }

    /// How many parameters a call to the member accepts.
    pub fn arity(self) -> Arity {
        TEXT_MEMBERS
            .iter()
            .find(|spec| spec.member == self)
            .map(|spec| spec.arity)
            .unwrap_or(Arity::AtLeast(0))
    }

    /// Calls the member on `target` with `parameters`.
    ///
    /// # Errors
    ///
    /// [`StringError::ParameterCount`] when the call gives a count the member
    /// does not accept, [`StringError::Convert`] when a parameter has no
    /// conversion the rule needs, [`StringError::FormatString`] when a format
    /// string names no field, and [`StringError::LengthOutOfRange`] when the
    /// answer cannot be held.
    pub fn call(self, target: &TjsString, parameters: &[Variant]) -> Result<Variant, StringError> {
        let arity = self.arity();
        if !arity.accepts(parameters.len()) {
            return Err(StringError::ParameterCount { arity });
        }
        match self {
            Self::CharAt => char_at(target, parameter(parameters, 0, arity)?),
            Self::IndexOf => index_of(target, parameters, arity),
            Self::ToUpperCase => Ok(Variant::String(case_fold(target, true))),
            Self::ToLowerCase => Ok(Variant::String(case_fold(target, false))),
            Self::Substring | Self::Substr => substring(target, parameters, arity),
            Self::Sprintf => Ok(Variant::String(format_string(target, parameters)?)),
            Self::Replace => replace(target, parameters),
            Self::Escape => Ok(Variant::String(target.escape_c())),
            Self::Split => split(target, parameters),
            Self::Trim => Ok(Variant::String(trim(target))),
            Self::Reverse => Ok(Variant::String(reverse(target))),
            Self::Repeat => repeat(target, parameter(parameters, 0, arity)?),
        }
    }
}

/// A member of a byte string's method surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OctetMember {
    /// The values the bytes rebuild under a format.
    Unpack,
}

impl OctetMember {
    /// Every member, in the order the engine's own name list has them.
    pub const ALL: &'static [Self] = &[Self::Unpack];

    /// The member `name` denotes, or `None` when it names no member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        let units = name.as_utf16();
        OCTET_MEMBERS
            .iter()
            .find(|spec| units.iter().copied().eq(spec.name.encode_utf16()))
            .map(|spec| spec.member)
    }

    /// The name the member is published under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unpack => "unpack",
        }
    }

    /// How many parameters a call to the member accepts.
    pub fn arity(self) -> Arity {
        OCTET_MEMBERS
            .iter()
            .find(|spec| spec.member == self)
            .map(|spec| spec.arity)
            .unwrap_or(Arity::AtLeast(0))
    }

    /// Calls the member on `target` with `parameters`.
    ///
    /// # Errors
    ///
    /// [`OctetError::ParameterCount`] when the call gives no format,
    /// [`OctetError::InvalidParameter`] when the format is not a text,
    /// [`OctetError::Pack`] when the bytes do not unpack under it, and
    /// [`OctetError::Array`] when the answer cannot hold the values.
    pub fn call(self, target: &Octet, parameters: &[Variant]) -> Result<Variant, OctetError> {
        let arity = self.arity();
        if !arity.accepts(parameters.len()) {
            return Err(OctetError::ParameterCount { arity });
        }
        match self {
            Self::Unpack => {
                let format = parameters
                    .first()
                    .ok_or(OctetError::ParameterCount { arity })?;
                let Variant::String(format) = format else {
                    return Err(OctetError::InvalidParameter { parameter: 0 });
                };
                let values = octet_pack::unpack(format, target)?;
                let array = Rc::new(ArrayObject::new());
                array.push(&values)?;
                let object: Rc<dyn Dispatch> = array;
                Ok(Variant::Object(Closure {
                    object: Some(object.clone()),
                    objthis: Some(object),
                }))
            }
        }
    }
}

/// One row of a value's member table.
struct MemberSpec<T> {
    /// The member the name denotes.
    member: T,
    /// The name the member is published under.
    name: &'static str,
    /// How many parameters a call accepts.
    arity: Arity,
}

/// One text member's row.
type TextSpec = MemberSpec<StringMember>;

/// One byte-string member's row.
type OctetSpec = MemberSpec<OctetMember>;

/// The text's table, in the order the engine's own name list has the names.
const TEXT_MEMBERS: &[TextSpec] = &[
    TextSpec {
        member: StringMember::CharAt,
        name: "charAt",
        arity: Arity::Exactly(1),
    },
    TextSpec {
        member: StringMember::IndexOf,
        name: "indexOf",
        arity: Arity::Between(1, 2),
    },
    TextSpec {
        member: StringMember::ToUpperCase,
        name: "toUpperCase",
        arity: Arity::Exactly(0),
    },
    TextSpec {
        member: StringMember::ToLowerCase,
        name: "toLowerCase",
        arity: Arity::Exactly(0),
    },
    TextSpec {
        member: StringMember::Substring,
        name: "substring",
        arity: Arity::Between(1, 2),
    },
    TextSpec {
        member: StringMember::Substr,
        name: "substr",
        arity: Arity::Between(1, 2),
    },
    TextSpec {
        member: StringMember::Sprintf,
        name: "sprintf",
        arity: Arity::AtLeast(0),
    },
    TextSpec {
        member: StringMember::Replace,
        name: "replace",
        arity: Arity::AtLeast(2),
    },
    TextSpec {
        member: StringMember::Escape,
        name: "escape",
        arity: Arity::AtLeast(0),
    },
    TextSpec {
        member: StringMember::Split,
        name: "split",
        arity: Arity::AtLeast(1),
    },
    TextSpec {
        member: StringMember::Trim,
        name: "trim",
        arity: Arity::Exactly(0),
    },
    TextSpec {
        member: StringMember::Reverse,
        name: "reverse",
        arity: Arity::Exactly(0),
    },
    TextSpec {
        member: StringMember::Repeat,
        name: "repeat",
        arity: Arity::Exactly(1),
    },
];

/// The byte string's table, in the order the engine's own name list has the
/// names.
const OCTET_MEMBERS: &[OctetSpec] = &[OctetSpec {
    member: OctetMember::Unpack,
    name: "unpack",
    arity: Arity::AtLeast(1),
}];

/// How many parameters a member's call accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arity {
    /// Exactly this many.
    Exactly(usize),
    /// At least the first, and no more than the second.
    Between(usize, usize),
    /// At least this many; the parameters after them are ignored.
    AtLeast(usize),
}

impl Arity {
    /// Whether a call with `count` parameters is one the member accepts.
    pub fn accepts(self, count: usize) -> bool {
        match self {
            Self::Exactly(exact) => count == exact,
            Self::Between(min, max) => min <= count && count <= max,
            Self::AtLeast(min) => min <= count,
        }
    }
}

/// The failure of a text member's call.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StringError {
    /// The call gives a number of parameters the member does not accept.
    #[error("the member does not accept this many parameters")]
    ParameterCount {
        /// The counts the member accepts.
        arity: Arity,
    },

    /// A parameter has no conversion the rule needs.
    #[error("the parameter cannot be converted")]
    Convert {
        /// The position of the parameter the rule read.
        parameter: usize,
        /// The conversion that failed.
        error: VariantError,
    },

    /// A format string names no field.
    #[error("the format string names no field")]
    FormatString,

    /// The text the call would answer cannot be held.
    #[error("the text is too long")]
    LengthOutOfRange,
}

impl StringError {
    /// The failure a parameter's conversion answers.
    fn convert(error: VariantError, parameter: usize) -> Self {
        Self::Convert { parameter, error }
    }
}

/// The failure of a byte-string member's call.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum OctetError {
    /// The call gives a number of parameters the member does not accept.
    #[error("the member does not accept this many parameters")]
    ParameterCount {
        /// The counts the member accepts.
        arity: Arity,
    },

    /// A parameter is not the kind the member's rule requires.
    #[error("the parameter is not a text")]
    InvalidParameter {
        /// The position of the parameter the rule read.
        parameter: usize,
    },

    /// The pack format refused the call.
    #[error("the pack format refused the call")]
    Pack(#[from] PackError),

    /// The answer could not hold the values.
    #[error("the answer cannot hold the values")]
    Array(#[from] ArrayError),
}

/// The parameter at `index`, or the count failure the member's arity names.
fn parameter(parameters: &[Variant], index: usize, arity: Arity) -> Result<&Variant, StringError> {
    parameters
        .get(index)
        .ok_or(StringError::ParameterCount { arity })
}

/// The integer the parameter at `parameter` is read as.
fn integer_of(value: &Variant, parameter: usize) -> Result<i64, StringError> {
    value
        .to_integer()
        .map_err(|error| StringError::convert(error, parameter))
}

/// The text the parameter at `parameter` is read as.
fn text_of(value: &Variant, parameter: usize) -> Result<TjsString, StringError> {
    value
        .to_string()
        .map_err(|error| StringError::convert(error, parameter))
}

/// `charAt`: the one unit at the position, or the empty text.
fn char_at(target: &TjsString, index: &Variant) -> Result<Variant, StringError> {
    let index = integer_of(index, 0)?;
    let units = target.as_utf16();
    let length = i64::try_from(units.len()).map_err(|_| StringError::LengthOutOfRange)?;
    if index < 0 || index >= length {
        return Ok(Variant::String(TjsString::new()));
    }
    let position = usize::try_from(index).map_err(|_| StringError::LengthOutOfRange)?;
    let unit = units.get(position).ok_or(StringError::LengthOutOfRange)?;
    Ok(Variant::String(TjsString::from(&[*unit][..])))
}

/// `indexOf`: the position of the text inside the text.
///
/// A start below zero is outside the text. The engine's search reads before the
/// text then, which has no defined answer; this reads from the front, and a
/// script whose start is inside `0 …= length` reads the engine's answer exactly.
fn index_of(
    target: &TjsString,
    parameters: &[Variant],
    arity: Arity,
) -> Result<Variant, StringError> {
    let needle = text_of(parameter(parameters, 0, arity)?, 0)?;
    // an empty text has no haystack and an empty needle has no length to find;
    // both answer the missing position
    if target.is_empty() || needle.is_empty() {
        return Ok(Variant::Integer(-1));
    }
    let start = match parameters.get(1) {
        Some(value) => integer_of(value, 1)?,
        None => 0,
    };
    let length = i64::try_from(target.len()).map_err(|_| StringError::LengthOutOfRange)?;
    if start >= length {
        return Ok(Variant::Integer(-1));
    }
    let start = if start < 0 {
        0
    } else {
        usize::try_from(start).map_err(|_| StringError::LengthOutOfRange)?
    };
    match find_from(target.as_utf16(), needle.as_utf16(), start) {
        Some(position) => {
            let position = i64::try_from(position).map_err(|_| StringError::LengthOutOfRange)?;
            Ok(Variant::Integer(position))
        }
        None => Ok(Variant::Integer(-1)),
    }
}

/// The position of the first `needle` in `haystack` at or after `start`.
fn find_from(haystack: &[u16], needle: &[u16], start: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    (start..=last).find(|&position| haystack[position..position + needle.len()] == *needle)
}

/// `substring` and `substr`: the text from a position, or from it for a count.
fn substring(
    target: &TjsString,
    parameters: &[Variant],
    arity: Arity,
) -> Result<Variant, StringError> {
    let start = integer_of(parameter(parameters, 0, arity)?, 0)?;
    let units = target.as_utf16();
    let length = i64::try_from(units.len()).map_err(|_| StringError::LengthOutOfRange)?;
    if start < 0 || start >= length {
        return Ok(Variant::String(TjsString::new()));
    }
    let start = usize::try_from(start).map_err(|_| StringError::LengthOutOfRange)?;
    let remaining = units.len() - start;
    let count = match parameters.get(1) {
        None => remaining,
        Some(value) => {
            let count = integer_of(value, 1)?;
            if count < 0 {
                return Ok(Variant::String(TjsString::new()));
            }
            let count = usize::try_from(count).map_err(|_| StringError::LengthOutOfRange)?;
            // a count past the end is the rest of the text
            count.min(remaining)
        }
    };
    let end = start
        .checked_add(count)
        .ok_or(StringError::LengthOutOfRange)?;
    let text = units.get(start..end).ok_or(StringError::LengthOutOfRange)?;
    Ok(Variant::String(TjsString::from(text)))
}

/// `toUpperCase` and `toLowerCase`: the ASCII letters folded.
fn case_fold(target: &TjsString, upper: bool) -> TjsString {
    let units: Vec<u16> = target
        .as_utf16()
        .iter()
        .map(|&unit| {
            let letter = if upper {
                matches!(unit, 0x61..=0x7a)
            } else {
                matches!(unit, 0x41..=0x5a)
            };
            if letter {
                if upper { unit - 0x20 } else { unit + 0x20 }
            } else {
                unit
            }
        })
        .collect();
    TjsString::from(units)
}

/// `trim`: the text with the units from one to `0x20` cut from both ends.
fn trim(target: &TjsString) -> TjsString {
    let units = target.as_utf16();
    let Some(first) = units.iter().position(|&unit| !is_blank(unit)) else {
        return TjsString::new();
    };
    let Some(last) = units.iter().rposition(|&unit| !is_blank(unit)) else {
        return TjsString::new();
    };
    TjsString::from(&units[first..=last])
}

/// Whether a unit is one of the blanks `trim` cuts.
fn is_blank(unit: u16) -> bool {
    (0x01..=0x20).contains(&unit)
}

/// `reverse`: the text with its units in the other order.
fn reverse(target: &TjsString) -> TjsString {
    let units: Vec<u16> = target.as_utf16().iter().rev().copied().collect();
    TjsString::from(units)
}

/// `repeat`: the text the given number of times.
fn repeat(target: &TjsString, count: &Variant) -> Result<Variant, StringError> {
    let count = integer_of(count, 0)?;
    if count <= 0 || target.is_empty() {
        return Ok(Variant::String(TjsString::new()));
    }
    let count = usize::try_from(count).map_err(|_| StringError::LengthOutOfRange)?;
    let length = target
        .len()
        .checked_mul(count)
        .ok_or(StringError::LengthOutOfRange)?;
    let mut units = Vec::with_capacity(length);
    for _ in 0..count {
        units.extend_from_slice(target.as_utf16());
    }
    Ok(Variant::String(TjsString::from(units)))
}

/// `replace`: the pattern value's `replace`, with the text and the replacement.
///
/// The pattern is read as an object; every other kind has no object form and is
/// a conversion failure. The engine does not test the object's answer, so a
/// pattern that cannot serve the call leaves the destination as it was, which is
/// the empty value.
fn replace(target: &TjsString, parameters: &[Variant]) -> Result<Variant, StringError> {
    let pattern = &parameters[0];
    let Variant::Object(closure) = pattern else {
        return Err(StringError::convert(
            VariantError::Convert { to: "Object" },
            0,
        ));
    };
    let Some(object) = closure.object.as_deref() else {
        return Ok(Variant::Void);
    };
    let arguments = [Variant::String(target.clone()), parameters[1].clone()];
    match object.func_call(
        &TjsString::from("replace"),
        Some(&arguments),
        closure.objthis.as_deref(),
    ) {
        Ok(answer) => Ok(answer),
        Err(_refused) => Ok(Variant::Void),
    }
}

/// `split`: a fresh array's `split`, with the text after the pattern.
///
/// The pattern, the text and at most the two parameters after the pattern are
/// handed to the array, and the array is the answer whatever the member did with
/// them: the engine does not test the array's answer.
fn split(target: &TjsString, parameters: &[Variant]) -> Result<Variant, StringError> {
    let array = Rc::new(ArrayObject::new());
    let object: Rc<dyn Dispatch> = array;
    let value = Variant::Object(Closure {
        object: Some(object.clone()),
        objthis: Some(object.clone()),
    });
    let mut arguments = Vec::with_capacity(4);
    arguments.push(parameters[0].clone());
    arguments.push(Variant::String(target.clone()));
    arguments.extend(parameters.iter().skip(1).take(2).cloned());
    match object.func_call(
        &TjsString::from("split"),
        Some(&arguments),
        Some(object.as_ref()),
    ) {
        Ok(_answer) => {}
        Err(_refused) => {}
    }
    Ok(value)
}

/// The field marker of a format string.
const PERCENT: u16 = b'%' as u16;
/// The left-alignment flag.
const FLAG_LEFT: u16 = b'-' as u16;
/// The sign flag.
const FLAG_PLUS: u16 = b'+' as u16;
/// The alternative-form flag.
const FLAG_ALTERNATE: u16 = b'#' as u16;
/// The zero-padding flag.
const FLAG_ZERO: u16 = b'0' as u16;
/// The width-from-a-parameter marker.
const STAR: u16 = b'*' as u16;
/// The precision marker.
const POINT: u16 = b'.' as u16;
/// The longest field the engine builds: a width and a precision past this total
/// are a format failure rather than an answer.
const LONGEST_FIELD: i64 = 900;

/// `sprintf`: the text the format string builds from the parameters.
///
/// A field is a `%`, one optional flag out of `-`, `+` and `#`, an optional `0`,
/// a width (`*` or digits), a precision (`.*` or `.digits`) and a type unit. The
/// type units are `%`, `c`, `s`, the integer kinds `d`, `i`, `o`, `u`, `x` and
/// `X`, and the real kinds `f`, `e`, `g`, `E` and `G`. A `%` with no type unit
/// after it is a failure, and so is a field whose width and precision together
/// are longer than the engine's own bound; a unit that names no kind is written
/// as itself, with the field's modifiers dropped.
fn format_string(format: &TjsString, parameters: &[Variant]) -> Result<TjsString, StringError> {
    let units = format.as_utf16();
    let mut out: Vec<u16> = Vec::with_capacity(units.len());
    let mut next = 0usize;
    let mut at = 0usize;
    while at < units.len() {
        let unit = units[at];
        if unit != PERCENT {
            out.push(unit);
            at += 1;
            continue;
        }
        at += 1;
        let flag = match units.get(at) {
            Some(&flag) if flag == FLAG_LEFT || flag == FLAG_PLUS || flag == FLAG_ALTERNATE => {
                at += 1;
                Some(flag)
            }
            Some(_) => None,
            None => return Err(StringError::FormatString),
        };
        let zero = if units.get(at) == Some(&FLAG_ZERO) {
            at += 1;
            true
        } else {
            false
        };
        let width = match units.get(at) {
            None => return Err(StringError::FormatString),
            Some(&unit) if unit == STAR => {
                at += 1;
                let width = integer_of(parameter(parameters, next, Arity::AtLeast(0))?, next)?;
                next += 1;
                if width < 0 {
                    return Err(StringError::FormatString);
                }
                width
            }
            Some(_) => {
                let (width, end) = digits_of(units, at)?;
                at = end;
                width
            }
        };
        let precision = match units.get(at) {
            Some(&unit) if unit == POINT => {
                at += 1;
                match units.get(at) {
                    Some(&unit) if unit == STAR => {
                        at += 1;
                        let precision =
                            integer_of(parameter(parameters, next, Arity::AtLeast(0))?, next)?;
                        next += 1;
                        if precision < 0 { None } else { Some(precision) }
                    }
                    Some(&unit) if is_digit(unit) => {
                        let (precision, end) = digits_of(units, at)?;
                        at = end;
                        Some(precision)
                    }
                    _ => return Err(StringError::FormatString),
                }
            }
            _ => None,
        };
        let Some(&kind) = units.get(at) else {
            return Err(StringError::FormatString);
        };
        at += 1;
        if kind == PERCENT {
            out.push(PERCENT);
            continue;
        }
        if kind == u16::from(b'c') || kind == u16::from(b's') {
            let text = text_of(parameter(parameters, next, Arity::AtLeast(0))?, next)?;
            next += 1;
            let field = text_field(&text, kind == u16::from(b'c'), flag, width, precision)?;
            out.extend(field);
            continue;
        }
        if is_integer_kind(kind) {
            let value = integer_of(parameter(parameters, next, Arity::AtLeast(0))?, next)?;
            next += 1;
            let field = integer_field(kind, value, flag, zero, width, precision)?;
            out.extend(field.encode_utf16());
            continue;
        }
        if is_real_kind(kind) {
            let argument = parameter(parameters, next, Arity::AtLeast(0))?;
            let value = argument
                .to_real()
                .map_err(|error| StringError::convert(error, next))?;
            next += 1;
            let field = real_field(kind, value, flag, zero, width, precision)?;
            out.extend(field.encode_utf16());
            continue;
        }
        // a type unit that names no kind is written as itself
        out.push(kind);
    }
    Ok(TjsString::from(out))
}

/// The digits a field's width or precision is written as.
fn digits_of(units: &[u16], start: usize) -> Result<(i64, usize), StringError> {
    let mut value: i64 = 0;
    let mut at = start;
    while let Some(&unit) = units.get(at) {
        if !is_digit(unit) {
            break;
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(unit - FLAG_ZERO)))
            .ok_or(StringError::FormatString)?;
        at += 1;
    }
    Ok((value, at))
}

/// Whether a unit is one of the decimal digits.
fn is_digit(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39)
}

/// Whether a type unit names one of the integer kinds.
fn is_integer_kind(kind: u16) -> bool {
    matches!(kind, k if k == u16::from(b'd')
        || k == u16::from(b'i')
        || k == u16::from(b'o')
        || k == u16::from(b'u')
        || k == u16::from(b'x')
        || k == u16::from(b'X'))
}

/// Whether a type unit names one of the real kinds.
fn is_real_kind(kind: u16) -> bool {
    matches!(kind, k if k == u16::from(b'f')
        || k == u16::from(b'e')
        || k == u16::from(b'E')
        || k == u16::from(b'g')
        || k == u16::from(b'G'))
}

/// The units a `c` or an `s` field writes.
///
/// The value is read as a text and cut to the precision, and the field is the
/// width wide with spaces. A `c` reads one unit of the text.
fn text_field(
    text: &TjsString,
    character: bool,
    flag: Option<u16>,
    width: i64,
    precision: Option<i64>,
) -> Result<Vec<u16>, StringError> {
    let length = text.len();
    let mut cut = match precision {
        Some(precision) => usize::try_from(precision).map_err(|_| StringError::FormatString)?,
        None => length,
    };
    if cut == 0 {
        cut = length;
    }
    if character && cut > 1 {
        cut = 1;
    }
    let cut = cut.min(length);
    let width = i64::try_from(cut).map_or(width, |cut| width.max(cut));
    let width = usize::try_from(width).map_err(|_| StringError::FormatString)?;
    let pad = width.saturating_sub(cut);
    let mut units = Vec::with_capacity(width);
    if flag == Some(FLAG_LEFT) {
        units.extend_from_slice(&text.as_utf16()[..cut]);
        units.extend(std::iter::repeat_n(u16::from(b' '), pad));
    } else {
        units.extend(std::iter::repeat_n(u16::from(b' '), pad));
        units.extend_from_slice(&text.as_utf16()[..cut]);
    }
    Ok(units)
}

/// The text of one integer field.
fn integer_field(
    kind: u16,
    value: i64,
    flag: Option<u16>,
    zero: bool,
    width: i64,
    precision: Option<i64>,
) -> Result<String, StringError> {
    if width > 0 && width.saturating_add(precision.unwrap_or(0)) > LONGEST_FIELD {
        return Err(StringError::FormatString);
    }
    let precision = match precision {
        Some(precision) => Some(usize::try_from(precision).map_err(|_| StringError::FormatString)?),
        None => None,
    };
    // a precision decides the zeros, so the zero-padding flag does not apply
    let zero = zero && precision.is_none();
    let negative = value < 0;
    // a signed field prints the magnitude with a sign; an unsigned one prints the
    // value's bits, so -1 is the largest 64-bit value rather than 1
    let magnitude = if kind == u16::from(b'd') || kind == u16::from(b'i') {
        value.unsigned_abs()
    } else {
        u64::from_ne_bytes(value.to_ne_bytes())
    };
    if kind == u16::from(b'd') || kind == u16::from(b'i') {
        let digits = padded_digits(magnitude.to_string(), precision);
        let sign = if negative {
            "-"
        } else if flag == Some(FLAG_PLUS) {
            "+"
        } else {
            ""
        };
        return padded(sign, "", &digits, width, zero, flag == Some(FLAG_LEFT));
    }
    let digits = match kind {
        k if k == u16::from(b'o') => format!("{magnitude:o}"),
        k if k == u16::from(b'x') => format!("{magnitude:x}"),
        k if k == u16::from(b'X') => format!("{magnitude:X}"),
        _ => magnitude.to_string(),
    };
    let digits = padded_digits(digits, precision);
    let prefix = match kind {
        k if k == u16::from(b'o') => {
            if flag == Some(FLAG_ALTERNATE) && !digits.starts_with('0') {
                "0"
            } else {
                ""
            }
        }
        k if k == u16::from(b'x') && flag == Some(FLAG_ALTERNATE) && magnitude != 0 => "0x",
        k if k == u16::from(b'X') && flag == Some(FLAG_ALTERNATE) && magnitude != 0 => "0X",
        _ => "",
    };
    padded("", prefix, &digits, width, zero, flag == Some(FLAG_LEFT))
}

/// The digits of an integer field with the precision's zero padding.
fn padded_digits(digits: String, precision: Option<usize>) -> String {
    let Some(precision) = precision else {
        return digits;
    };
    if digits == "0" && precision == 0 {
        return String::new();
    }
    if digits.len() >= precision {
        return digits;
    }
    let mut padded = String::with_capacity(precision);
    padded.extend(std::iter::repeat_n('0', precision - digits.len()));
    padded.push_str(&digits);
    padded
}

/// The text of one real field.
fn real_field(
    kind: u16,
    value: f64,
    flag: Option<u16>,
    zero: bool,
    width: i64,
    precision: Option<i64>,
) -> Result<String, StringError> {
    if width > 0 && width.saturating_add(precision.unwrap_or(0)) > LONGEST_FIELD {
        return Err(StringError::FormatString);
    }
    let upper = kind == u16::from(b'E') || kind == u16::from(b'G');
    let alternate = flag == Some(FLAG_ALTERNATE);
    let body = if value.is_nan() {
        String::from(if upper { "NAN" } else { "nan" })
    } else if value.is_infinite() {
        String::from(if upper { "INF" } else { "inf" })
    } else {
        let precision = match kind {
            k if k == u16::from(b'g') || k == u16::from(b'G') => match precision {
                Some(0) | None => 6,
                Some(precision) => precision,
            },
            _ => precision.unwrap_or(6),
        };
        let precision = usize::try_from(precision).map_err(|_| StringError::FormatString)?;
        let magnitude = value.abs();
        let text = match kind {
            k if k == u16::from(b'f') => fixed(magnitude, precision, alternate),
            k if k == u16::from(b'e') || k == u16::from(b'E') => {
                scientific(magnitude, precision, alternate)?
            }
            _ => general(magnitude, precision, alternate)?,
        };
        if upper { text.to_uppercase() } else { text }
    };
    let sign = if value.is_sign_negative() {
        "-"
    } else if flag == Some(FLAG_PLUS) {
        "+"
    } else {
        ""
    };
    padded(sign, "", &body, width, zero, flag == Some(FLAG_LEFT))
}

/// The fixed-point form of a real.
fn fixed(value: f64, precision: usize, alternate: bool) -> String {
    if precision == 0 && alternate {
        return format!("{value:.0}.");
    }
    format!("{value:.precision$}")
}

/// The scientific form of a real, with an exponent of at least two digits.
fn scientific(value: f64, precision: usize, alternate: bool) -> Result<String, StringError> {
    let text = format!("{value:.precision$e}");
    let (mantissa, exponent) = text.split_once('e').ok_or(StringError::FormatString)?;
    let exponent: i32 = exponent
        .parse()
        .map_err(|_error| StringError::FormatString)?;
    let point = if precision == 0 && alternate { "." } else { "" };
    Ok(format!("{mantissa}{point}{}", exponent_field(exponent)))
}

/// The exponent part of a scientific field: a sign, then at least two digits.
fn exponent_field(exponent: i32) -> String {
    let sign = if exponent < 0 { '-' } else { '+' };
    format!("e{sign}{:02}", exponent.unsigned_abs())
}

/// The general form of a real: the fixed form or the scientific one, by the
/// exponent, with the trailing zeros cut unless the alternative form asks for
/// them.
fn general(value: f64, precision: usize, alternate: bool) -> Result<String, StringError> {
    let exponent = if value == 0.0 {
        0i32
    } else {
        magnitude_exponent(value)?
    };
    let precision_i64 = i64::try_from(precision).map_err(|_| StringError::FormatString)?;
    if i64::from(exponent) < -4 || i64::from(exponent) >= precision_i64 {
        let digits = precision.saturating_sub(1);
        let text = scientific(value, digits, alternate)?;
        return Ok(if alternate { text } else { strip_zeros(&text) });
    }
    let digits = precision_i64 - 1 - i64::from(exponent);
    let digits = usize::try_from(digits).map_err(|_| StringError::FormatString)?;
    let text = fixed(value, digits, alternate);
    Ok(if alternate { text } else { strip_zeros(&text) })
}

/// The decimal exponent a real's scientific form reads as.
fn magnitude_exponent(value: f64) -> Result<i32, StringError> {
    let text = format!("{value:e}");
    let (_, exponent) = text.split_once('e').ok_or(StringError::FormatString)?;
    exponent.parse().map_err(|_error| StringError::FormatString)
}

/// The text with the fraction's trailing zeros cut, keeping an exponent when the
/// text has one.
fn strip_zeros(text: &str) -> String {
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (text, None),
    };
    let mut mantissa = String::from(mantissa);
    if mantissa.contains('.') {
        while mantissa.ends_with('0') {
            mantissa.pop();
        }
        if mantissa.ends_with('.') {
            mantissa.pop();
        }
    }
    match exponent {
        Some(exponent) => format!("{mantissa}e{exponent}"),
        None => mantissa,
    }
}

/// A field's text with its sign, its prefix and its width.
fn padded(
    sign: &str,
    prefix: &str,
    body: &str,
    width: i64,
    zero: bool,
    left: bool,
) -> Result<String, StringError> {
    let used = sign.len() + prefix.len() + body.len();
    let width = usize::try_from(width).map_err(|_| StringError::FormatString)?;
    if width <= used {
        return Ok(format!("{sign}{prefix}{body}"));
    }
    let pad = width - used;
    if left {
        return Ok(format!("{sign}{prefix}{body}{}", " ".repeat(pad)));
    }
    if zero {
        return Ok(format!("{sign}{prefix}{}{body}", "0".repeat(pad)));
    }
    Ok(format!("{}{sign}{prefix}{body}", " ".repeat(pad)))
}

#[cfg(test)]
mod tests {
    use super::{OctetMember, StringError, StringMember};
    use crate::vm::runtime::octet::Octet;
    use crate::vm::runtime::string::TjsString;
    use crate::vm::runtime::variant::{Closure, Variant, VariantError};

    /// A member's answer as `kind: text`, so one assertion checks both.
    fn answered(answer: Result<Variant, StringError>) -> String {
        let value = answer.expect("the member answers");
        let text = value.to_string().expect("the value has a text");
        format!("{}: {text}", value.type_name())
    }

    fn text(value: &str) -> TjsString {
        TjsString::from(value)
    }

    /// The table is the engine's name list: thirteen text members and one byte
    /// string member, in that order.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2254-2282
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2586-2604
    #[test]
    fn the_tables_are_the_engines_name_lists() {
        let text_names: Vec<&str> = StringMember::ALL
            .iter()
            .map(|member| member.name())
            .collect();
        assert_eq!(
            text_names,
            vec![
                "charAt",
                "indexOf",
                "toUpperCase",
                "toLowerCase",
                "substring",
                "substr",
                "sprintf",
                "replace",
                "escape",
                "split",
                "trim",
                "reverse",
                "repeat",
            ]
        );
        for member in StringMember::ALL {
            assert_eq!(StringMember::lookup(&text(member.name())), Some(*member));
        }
        assert_eq!(StringMember::lookup(&text("length")), None);
        assert_eq!(StringMember::lookup(&text("toString")), None);
        let octet_names: Vec<&str> = OctetMember::ALL
            .iter()
            .map(|member| member.name())
            .collect();
        assert_eq!(octet_names, vec!["unpack"]);
        assert_eq!(
            OctetMember::lookup(&text("unpack")),
            Some(OctetMember::Unpack)
        );
        assert_eq!(OctetMember::lookup(&text("pack")), None);
    }

    /// The arities are the engine's: an exact count, a range, or a minimum with
    /// the rest ignored.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2304-2585
    #[test]
    fn the_arities_are_the_engines() {
        assert!(StringMember::CharAt.arity().accepts(1));
        assert!(!StringMember::CharAt.arity().accepts(0));
        assert!(!StringMember::CharAt.arity().accepts(2));
        assert!(StringMember::IndexOf.arity().accepts(1));
        assert!(StringMember::IndexOf.arity().accepts(2));
        assert!(!StringMember::IndexOf.arity().accepts(3));
        assert!(!StringMember::ToUpperCase.arity().accepts(1));
        assert!(StringMember::ToUpperCase.arity().accepts(0));
        assert!(StringMember::Sprintf.arity().accepts(0));
        assert!(StringMember::Sprintf.arity().accepts(9));
        assert!(StringMember::Replace.arity().accepts(2));
        assert!(!StringMember::Replace.arity().accepts(1));
        assert!(StringMember::Split.arity().accepts(1));
        assert!(!StringMember::Split.arity().accepts(0));
        assert!(OctetMember::Unpack.arity().accepts(1));
        assert!(!OctetMember::Unpack.arity().accepts(0));
    }

    /// `charAt` answers one unit inside the text and the empty text outside it.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2304-2330
    #[test]
    fn char_at_answers_one_unit() {
        let target = text("abc");
        assert_eq!(
            answered(StringMember::CharAt.call(&target, &[Variant::from(1i64)])),
            "String: b"
        );
        assert_eq!(
            answered(StringMember::CharAt.call(&target, &[Variant::from(3i64)])),
            "String: "
        );
        assert_eq!(
            answered(StringMember::CharAt.call(&target, &[Variant::from(-1i64)])),
            "String: "
        );
        assert_eq!(
            answered(StringMember::CharAt.call(&text(""), &[Variant::from(0i64)])),
            "String: "
        );
        assert_eq!(
            StringMember::CharAt.call(&target, &[]).unwrap_err(),
            StringError::ParameterCount {
                arity: StringMember::CharAt.arity()
            }
        );
    }

    /// `indexOf` answers the first position, and the empty text and the empty
    /// needle are both "not found".
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2331-2375
    #[test]
    fn index_of_answers_the_first_position() {
        let target = text("abcabc");
        assert_eq!(
            answered(StringMember::IndexOf.call(&target, &[Variant::from("b")])),
            "Integer: 1"
        );
        assert_eq!(
            answered(
                StringMember::IndexOf.call(&target, &[Variant::from("b"), Variant::from(2i64)])
            ),
            "Integer: 4"
        );
        assert_eq!(
            answered(StringMember::IndexOf.call(&target, &[Variant::from("z")])),
            "Integer: -1"
        );
        assert_eq!(
            answered(StringMember::IndexOf.call(&target, &[Variant::from("")])),
            "Integer: -1"
        );
        assert_eq!(
            answered(StringMember::IndexOf.call(&text(""), &[Variant::from("a")])),
            "Integer: -1"
        );
        assert_eq!(
            answered(StringMember::IndexOf.call(&text("a1b"), &[Variant::from(1i64)])),
            "Integer: 1"
        );
    }

    /// The two case rules fold the ASCII letters only.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2376-2421
    #[test]
    fn the_case_rules_fold_ascii_only() {
        assert_eq!(
            answered(StringMember::ToUpperCase.call(&text("aBc1\u{00e9}"), &[])),
            "String: ABC1\u{00e9}"
        );
        assert_eq!(
            answered(StringMember::ToLowerCase.call(&text("AbC1\u{00c9}"), &[])),
            "String: abc1\u{00c9}"
        );
    }

    /// `substring` and `substr` answer the rest of the text for a start and a
    /// count, and the empty text outside it.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2422-2448
    #[test]
    fn substring_answers_a_range() {
        let target = text("abcdef");
        assert_eq!(
            answered(StringMember::Substring.call(&target, &[Variant::from(2i64)])),
            "String: cdef"
        );
        assert_eq!(
            answered(
                StringMember::Substring.call(&target, &[Variant::from(2i64), Variant::from(3i64)])
            ),
            "String: cde"
        );
        assert_eq!(
            answered(
                StringMember::Substr.call(&target, &[Variant::from(1i64), Variant::from(2i64)])
            ),
            "String: bc"
        );
        assert_eq!(
            answered(
                StringMember::Substring.call(&target, &[Variant::from(2i64), Variant::from(99i64)])
            ),
            "String: cdef"
        );
        assert_eq!(
            answered(
                StringMember::Substring.call(&target, &[Variant::from(2i64), Variant::from(-1i64)])
            ),
            "String: "
        );
        assert_eq!(
            answered(StringMember::Substring.call(&target, &[Variant::from(6i64)])),
            "String: "
        );
        assert_eq!(
            answered(StringMember::Substring.call(&target, &[Variant::from(-1i64)])),
            "String: "
        );
    }

    /// `escape` writes the named escapes, and `trim`, `reverse` and `repeat` are
    /// the three whole-text rules.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2485-2585
    /// reference/cpp/core/tjs2/tjsString.cpp:168-186
    /// reference/cpp/core/tjs2/tjsString.cpp:250-271
    #[test]
    fn the_whole_text_rules() {
        assert_eq!(
            answered(StringMember::Escape.call(&text("a\tb"), &[])),
            "String: a\\tb"
        );
        assert_eq!(
            answered(StringMember::Trim.call(&text(" \t ab \n"), &[])),
            "String: ab"
        );
        assert_eq!(
            answered(StringMember::Trim.call(&text("   "), &[])),
            "String: "
        );
        assert_eq!(
            answered(StringMember::Reverse.call(&text("abc"), &[])),
            "String: cba"
        );
        assert_eq!(
            answered(StringMember::Repeat.call(&text("ab"), &[Variant::from(3i64)])),
            "String: ababab"
        );
        assert_eq!(
            answered(StringMember::Repeat.call(&text("ab"), &[Variant::from(0i64)])),
            "String: "
        );
        assert_eq!(
            answered(StringMember::Repeat.call(&text("ab"), &[Variant::from(-1i64)])),
            "String: "
        );
    }

    /// `sprintf` builds the fields the format names, and a format that names no
    /// field is a failure.
    ///
    /// reference/cpp/core/tjs2/tjsVariantString.cpp:664-1045
    #[test]
    fn sprintf_builds_the_fields() {
        assert_eq!(
            answered(StringMember::Sprintf.call(
                &text("%d and %s"),
                &[Variant::from(5i64), Variant::from("x")]
            )),
            "String: 5 and x"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("[%5d]"), &[Variant::from(42i64)])),
            "String: [   42]"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("[%05d]"), &[Variant::from(-42i64)])),
            "String: [-0042]"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("[%-5d]"), &[Variant::from(42i64)])),
            "String: [42   ]"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("%+d"), &[Variant::from(42i64)])),
            "String: +42"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(
                &text("%x %X"),
                &[Variant::from(255i64), Variant::from(255i64)]
            )),
            "String: ff FF"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(
                &text("%#x %#o"),
                &[Variant::from(255i64), Variant::from(8i64)]
            )),
            "String: 0xff 010"
        );
        assert_eq!(
            answered(
                StringMember::Sprintf.call(&text("%.2f"), &[Variant::from(std::f64::consts::PI)])
            ),
            "String: 3.14"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("[%8.3f]"), &[Variant::from(1.5f64)])),
            "String: [   1.500]"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("%.3e"), &[Variant::from(12345.678f64)])),
            "String: 1.235e+04"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("%g"), &[Variant::from(0.0001f64)])),
            "String: 0.0001"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("%d%%"), &[Variant::from(50i64)])),
            "String: 50%"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("%c"), &[Variant::from(65i64)])),
            "String: 6"
        );
        assert_eq!(
            answered(StringMember::Sprintf.call(&text("%.2s"), &[Variant::from("abcdef")])),
            "String: ab"
        );
        assert_eq!(
            StringMember::Sprintf.call(&text("%"), &[]).unwrap_err(),
            StringError::FormatString
        );
        assert_eq!(
            StringMember::Sprintf.call(&text("%d"), &[]).unwrap_err(),
            StringError::ParameterCount {
                arity: StringMember::Sprintf.arity()
            }
        );
    }

    /// A pattern that is not an object has no object form, and a pattern that is
    /// the null object cannot serve the call, which leaves the answer empty.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2459-2474
    #[test]
    fn replace_asks_the_pattern() {
        assert_eq!(
            StringMember::Replace
                .call(&text("abc"), &[Variant::from("b"), Variant::from("x")])
                .unwrap_err(),
            StringError::Convert {
                parameter: 0,
                error: VariantError::Convert { to: "Object" },
            }
        );
        let null = Variant::Object(Closure::null());
        assert_eq!(
            answered(StringMember::Replace.call(&text("abc"), &[null, Variant::from("x")])),
            "void: "
        );
    }

    /// `split` answers an array even when the array cannot serve the member.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2475-2504
    #[test]
    fn split_answers_an_array() {
        let answer = StringMember::Split
            .call(&text("a,b"), &[Variant::from(",")])
            .expect("the member answers");
        let Variant::Object(closure) = answer else {
            panic!("the answer is not an object");
        };
        let object = closure.object.expect("the answer is an array");
        assert!(
            object
                .class_names()
                .iter()
                .any(|name| name == &TjsString::from("Array"))
        );
    }

    /// The byte string's `unpack` answers the values the bytes rebuild, and its
    /// failures are the engine's three.
    ///
    /// reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2586-2604
    /// reference/cpp/core/tjs2/tjsOctPack.cpp:1024-1042
    #[test]
    fn unpack_answers_the_values() {
        let octet = Octet::from_slice(&[1, 2, 3]);
        let answer = OctetMember::Unpack
            .call(&octet, &[Variant::from("c3")])
            .expect("the member answers");
        let Variant::Object(closure) = answer else {
            panic!("the answer is not an object");
        };
        let object = closure.object.expect("the answer is an array");
        let count = object
            .prop_get(
                &text("count"),
                crate::vm::runtime::object::MemberFlags::NONE,
                None,
            )
            .expect("the array has a count");
        assert_eq!(count.to_integer(), Ok(3));
        assert_eq!(
            OctetMember::Unpack.call(&octet, &[]).unwrap_err(),
            super::OctetError::ParameterCount {
                arity: OctetMember::Unpack.arity()
            }
        );
        assert_eq!(
            OctetMember::Unpack
                .call(&octet, &[Variant::from(5i64)])
                .unwrap_err(),
            super::OctetError::InvalidParameter { parameter: 0 }
        );
        assert!(matches!(
            OctetMember::Unpack.call(&octet, &[Variant::from("q")]),
            Err(super::OctetError::Pack(_))
        ));
    }
}
