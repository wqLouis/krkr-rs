//! The variant: the value a script holds, and the language's operators.
//!
//! A value is one of six kinds: the empty variant (`void`), an integer, a
//! real, a text, an octet, or an object. The empty variant is the one an
//! unset member reads as; `null` is an *object* whose handle is absent, not
//! the empty variant, so the two are different values.
//!
//! A value is immutable: an operator answers a new value, and nothing here
//! changes in place. Every conversion and every operator names the rule it
//! follows, and a conversion the language does not define is a typed error —
//! the language raises there, and so does this module.
//!
//! # The one deliberate divergence
//!
//! A quotient that does not fit a 64-bit integer is a typed error here, where
//! the shipped engine divides into the hardware's trap: `i64::MIN / -1` and
//! `-i64::MIN` stop the process there. A script sees a raised error instead of
//! losing the process, and the error names the range it could not hold. Every
//! other rule is the language's, and the tests state it above their assertions.

use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;

use super::number::{Number, NumberError};
use super::object::Dispatch;
use super::octet::Octet;
use super::string::TjsString;

/// An object value: the object, and the receiver its members are called on.
///
/// Both handles are shared, so cloning an object value never copies the
/// object. An absent object is the language's `null`.
#[derive(Clone, Default)]
pub struct Closure {
    /// The object; `None` is `null`.
    pub object: Option<Rc<dyn Dispatch>>,
    /// The receiver, when it differs from the object.
    pub objthis: Option<Rc<dyn Dispatch>>,
}

impl Closure {
    /// The null object.
    pub fn null() -> Self {
        Self::default()
    }

    /// Whether the value is the null object.
    pub fn is_null(&self) -> bool {
        self.object.is_none()
    }

    /// The object form: the two handles.
    fn form(&self) -> TjsString {
        let object = match &self.object {
            // the data address, not the fat pointer: its metadata half is
            // Rust's, and the engine's form is one address per handle
            Some(object) => format!("{:p}", Rc::as_ptr(object) as *const ()),
            None => String::from("0x0"),
        };
        let objthis = match &self.objthis {
            Some(objthis) => format!("{:p}", Rc::as_ptr(objthis) as *const ()),
            None => String::from("0x0"),
        };
        TjsString::from(format!("(object {object}:{objthis})"))
    }
}

impl fmt::Debug for Closure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "Closure {{ object: {}, objthis: {} }}",
            self.object.is_some(),
            self.objthis.is_some()
        )
    }
}

/// A script value: one of the six kinds the language has.
#[derive(Clone, Default)]
pub enum Variant {
    /// The empty variant.
    #[default]
    Void,
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit real.
    Real(f64),
    /// A text.
    String(TjsString),
    /// A byte value.
    Octet(Octet),
    /// An object value.
    Object(Closure),
}

impl From<i64> for Variant {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<f64> for Variant {
    fn from(value: f64) -> Self {
        Self::Real(value)
    }
}

impl From<&str> for Variant {
    fn from(text: &str) -> Self {
        Self::String(TjsString::from(text))
    }
}

impl From<TjsString> for Variant {
    fn from(text: TjsString) -> Self {
        Self::String(text)
    }
}

impl From<Octet> for Variant {
    fn from(octet: Octet) -> Self {
        Self::Octet(octet)
    }
}

impl From<Closure> for Variant {
    fn from(closure: Closure) -> Self {
        Self::Object(closure)
    }
}

impl From<Number> for Variant {
    fn from(number: Number) -> Self {
        match number {
            Number::Integer(value) => Self::Integer(value),
            Number::Real(value) => Self::Real(value),
        }
    }
}

impl fmt::Debug for Variant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Void => formatter.write_str("Void"),
            Self::Integer(value) => write!(formatter, "Integer({value})"),
            Self::Real(value) => write!(formatter, "Real({value})"),
            Self::String(text) => write!(formatter, "String({text:?})"),
            Self::Octet(octet) => write!(formatter, "{octet:?}"),
            Self::Object(closure) => write!(formatter, "Object({closure:?})"),
        }
    }
}

impl Variant {
    /// The kind, in the spelling the `typeof` operator answers with.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Void => "void",
            Self::Integer(_) => "Integer",
            Self::Real(_) => "Real",
            Self::String(_) => "String",
            Self::Octet(_) => "Octet",
            Self::Object(_) => "Object",
        }
    }

    /// The value in the engine's readable form, as a message quotes it.
    ///
    /// The kind is written in parentheses and the value after it, and an
    /// object is written as its form. The text is cut to 512 code units; a cut
    /// text ends in `...`.
    pub fn readable(&self) -> TjsString {
        let form = match self {
            Self::Void => TjsString::from("()"),
            Self::Integer(value) => TjsString::from(format!("(int){value}")),
            Self::Real(value) => TjsString::from(format!("(real){}", Number::Real(*value))),
            Self::String(text) => {
                let mut form: Vec<u16> = "(string)\"".encode_utf16().collect();
                form.extend_from_slice(text.escape_c().as_utf16());
                form.push(u16::from(b'"'));
                TjsString::from(form)
            }
            Self::Octet(octet) => TjsString::from(format!("(octet)<% {octet} %>")),
            Self::Object(closure) => TjsString::from(format!("(object){}", closure.form())),
        };
        trim_readable(form)
    }

    /// The value as a text.
    ///
    /// The empty variant is the empty text, a number is its text, a text is
    /// itself, and an object is its form — the two handles it carries, and
    /// `0x0` for a handle it does not. An octet has no text.
    pub fn to_string(&self) -> Result<TjsString, VariantError> {
        match self {
            Self::Void => Ok(TjsString::new()),
            Self::Integer(value) => Ok(TjsString::from(&Number::Integer(*value))),
            Self::Real(value) => Ok(TjsString::from(&Number::Real(*value))),
            Self::String(text) => Ok(text.clone()),
            Self::Octet(_) => Err(VariantError::Convert {
                to: Target::Text.name(),
            }),
            Self::Object(closure) => Ok(closure.form()),
        }
    }

    /// The value as a number.
    ///
    /// The empty variant is the integer zero. A text is read with the
    /// language's rules, and a text that is not a number is the *integer*
    /// zero, not an error: that is the one rule a text's own conversion does
    /// not own. An object and an octet are not numbers.
    pub fn to_number(&self) -> Result<Number, VariantError> {
        match self {
            Self::Void => Ok(Number::Integer(0)),
            Self::Integer(value) => Ok(Number::Integer(*value)),
            Self::Real(value) => Ok(Number::Real(*value)),
            Self::String(text) => match text.to_number() {
                Ok(number) => Ok(number),
                Err(NumberError::NotANumber) => Ok(Number::Integer(0)),
                Err(NumberError::RealOutOfRange) => Err(VariantError::Convert {
                    to: Target::Number.name(),
                }),
            },
            Self::Octet(_) => Err(VariantError::Convert {
                to: Target::Number.name(),
            }),
            Self::Object(_) => Err(VariantError::Convert {
                to: Target::Number.name(),
            }),
        }
    }

    /// The value as an integer, with a real truncated toward zero.
    pub fn to_integer(&self) -> Result<i64, VariantError> {
        if matches!(self, Self::Object(_) | Self::Octet(_)) {
            return Err(VariantError::Convert {
                to: Target::Integer.name(),
            });
        }
        integer_of(self.to_number()?)
    }

    /// The value as a real.
    pub fn to_real(&self) -> Result<f64, VariantError> {
        if matches!(self, Self::Object(_) | Self::Octet(_)) {
            return Err(VariantError::Convert {
                to: Target::Real.name(),
            });
        }
        Ok(self.to_number()?.to_real())
    }

    /// The value as an octet.
    ///
    /// The empty variant is the empty octet, an octet is itself, and every
    /// other kind has no octet.
    pub fn to_octet(&self) -> Result<Octet, VariantError> {
        match self {
            Self::Void => Ok(Octet::new()),
            Self::Octet(octet) => Ok(octet.clone()),
            Self::Integer(_) | Self::Real(_) | Self::String(_) | Self::Object(_) => {
                Err(VariantError::Convert {
                    to: Target::Octet.name(),
                })
            }
        }
    }

    /// The value of `self + other`.
    ///
    /// A text operand concatenates instead of converting: the other operand's
    /// text follows this one's. Two octets concatenate their bytes, two
    /// integers add, and an empty variant beside a number keeps the number's
    /// kind. Everything else adds as a real.
    pub fn add(&self, other: &Self) -> Result<Self, VariantError> {
        if matches!(self, Self::String(_)) || matches!(other, Self::String(_)) {
            let left = self.to_string()?;
            let right = other.to_string()?;
            return Ok(Self::String(left.concat(&right)));
        }
        if let Self::Octet(left) = self
            && let Self::Octet(right) = other
        {
            return Ok(Self::Octet(Octet::from_two(
                left.as_slice(),
                right.as_slice(),
            )));
        }
        if let Self::Integer(left) = self
            && let Self::Integer(right) = other
        {
            return Ok(Self::Integer(left.wrapping_add(*right)));
        }
        if matches!(self, Self::Void)
            && let Self::Integer(value) = other
        {
            return Ok(Self::Integer(*value));
        }
        if matches!(self, Self::Void)
            && let Self::Real(value) = other
        {
            return Ok(Self::Real(*value));
        }
        if matches!(other, Self::Void) && matches!(self, Self::Integer(_) | Self::Real(_)) {
            return Ok(self.clone());
        }
        Ok(Self::Real(self.to_real()? + other.to_real()?))
    }

    /// The value of `self - other`: two integers subtract, every other pair
    /// subtracts as a real.
    pub fn subtract(&self, other: &Self) -> Result<Self, VariantError> {
        let left = self.to_number()?;
        let right = other.to_number()?;
        match (left, right) {
            (Number::Integer(left), Number::Integer(right)) => {
                Ok(Self::Integer(left.wrapping_sub(right)))
            }
            (left, right) => Ok(Self::Real(left.to_real() - right.to_real())),
        }
    }

    /// The value of `self * other`: two integers multiply, every other pair
    /// multiplies as a real.
    pub fn multiply(&self, other: &Self) -> Result<Self, VariantError> {
        let left = self.to_number()?;
        let right = other.to_number()?;
        match (left, right) {
            (Number::Integer(left), Number::Integer(right)) => {
                Ok(Self::Integer(left.wrapping_mul(right)))
            }
            (left, right) => Ok(Self::Real(left.to_real() * right.to_real())),
        }
    }

    /// The value of `self / other`: a real division, so a divisor of zero is
    /// an infinity or a not-a-number rather than an error.
    pub fn divide(&self, other: &Self) -> Result<Self, VariantError> {
        Ok(Self::Real(self.to_real()? / other.to_real()?))
    }

    /// The value of `self \ other`: an integer division, whose divisor of
    /// zero raises.
    pub fn idivide(&self, other: &Self) -> Result<Self, VariantError> {
        let left = self.to_integer()?;
        let right = other.to_integer()?;
        if right == 0 {
            return Err(VariantError::DivideByZero);
        }
        match left.checked_div(right) {
            Some(quotient) => Ok(Self::Integer(quotient)),
            None => Err(VariantError::IntegerOutOfRange),
        }
    }

    /// The value of `self % other`: the truncating remainder of two integers,
    /// whose divisor of zero raises.
    pub fn remainder(&self, other: &Self) -> Result<Self, VariantError> {
        let left = self.to_integer()?;
        let right = other.to_integer()?;
        if right == 0 {
            return Err(VariantError::DivideByZero);
        }
        match left.checked_rem(right) {
            Some(remainder) => Ok(Self::Integer(remainder)),
            None => Err(VariantError::IntegerOutOfRange),
        }
    }

    /// The value of `self & other`, on the two integers.
    pub fn bit_and(&self, other: &Self) -> Result<Self, VariantError> {
        Ok(Self::Integer(self.to_integer()? & other.to_integer()?))
    }

    /// The value of `self | other`, on the two integers.
    pub fn bit_or(&self, other: &Self) -> Result<Self, VariantError> {
        Ok(Self::Integer(self.to_integer()? | other.to_integer()?))
    }

    /// The value of `self ^ other`, on the two integers.
    pub fn bit_xor(&self, other: &Self) -> Result<Self, VariantError> {
        Ok(Self::Integer(self.to_integer()? ^ other.to_integer()?))
    }

    /// The value of `self << other`, on the two integers.
    pub fn shift_left(&self, other: &Self) -> Result<Self, VariantError> {
        Ok(Self::Integer(
            self.to_integer()?
                .wrapping_shl(shift_count(other.to_integer()?)),
        ))
    }

    /// The value of `self >> other`: an arithmetic shift, on the two
    /// integers.
    pub fn shift_right(&self, other: &Self) -> Result<Self, VariantError> {
        Ok(Self::Integer(
            self.to_integer()?
                .wrapping_shr(shift_count(other.to_integer()?)),
        ))
    }

    /// The value of `self + 1`.
    ///
    /// The empty variant increments to the integer one, a text increments to
    /// the number it reads as, and an octet or an object has no increment.
    pub fn increment(&self) -> Result<Self, VariantError> {
        match self {
            Self::Void => Ok(Self::Integer(1)),
            Self::Integer(value) => Ok(Self::Integer(value.wrapping_add(1))),
            Self::Real(value) => Ok(Self::Real(value + 1.0)),
            Self::String(_) => Self::from(self.to_number()?).increment(),
            Self::Octet(_) | Self::Object(_) => Err(VariantError::Convert {
                to: Target::Number.name(),
            }),
        }
    }

    /// The value of `self - 1`.
    ///
    /// The empty variant decrements to the integer minus one, a text
    /// decrements to the number it reads as, and an octet or an object has no
    /// decrement.
    pub fn decrement(&self) -> Result<Self, VariantError> {
        match self {
            Self::Void => Ok(Self::Integer(-1)),
            Self::Integer(value) => Ok(Self::Integer(value.wrapping_sub(1))),
            Self::Real(value) => Ok(Self::Real(value - 1.0)),
            Self::String(_) => Self::from(self.to_number()?).decrement(),
            Self::Octet(_) | Self::Object(_) => Err(VariantError::Convert {
                to: Target::Number.name(),
            }),
        }
    }

    /// The value of `-self`: the number the value reads as, negated.
    pub fn negate(&self) -> Result<Self, VariantError> {
        match self.to_number()? {
            Number::Integer(value) => Ok(Self::Integer(value.wrapping_neg())),
            Number::Real(value) => Ok(Self::Real(-value)),
        }
    }

    /// The value of `~self`: the complement of the integer the value reads
    /// as.
    pub fn complement(&self) -> Result<Self, VariantError> {
        Ok(Self::Integer(!self.to_integer()?))
    }

    /// Whether the two values are equal, as the `==` operator answers.
    ///
    /// Two values of one kind compare by that kind, and an object compares by
    /// its object handle. A text operand compares as text, so a number beside
    /// a text compares as the number's text. The empty variant equals the
    /// integer zero, the real zero and the empty text, and nothing else. A
    /// not-a-number is equal to nothing, and an octet is equal only to
    /// another octet.
    pub fn equals(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Void, Self::Void) => return true,
            (Self::Integer(left), Self::Integer(right)) => return left == right,
            (Self::Real(left), Self::Real(right)) => return left == right,
            (Self::Octet(left), Self::Octet(right)) => return left == right,
            (Self::String(left), Self::String(right)) => return left == right,
            (Self::Object(left), Self::Object(right)) => {
                return match (&left.object, &right.object) {
                    (Some(left), Some(right)) => Rc::ptr_eq(left, right),
                    (None, None) => true,
                    _ => false,
                };
            }
            _ => {}
        }
        // a text operand compares as text; a conversion the language refuses
        // makes the two unequal, because the comparison is asked for a truth
        if matches!(self, Self::String(_)) || matches!(other, Self::String(_)) {
            let (Ok(left), Ok(right)) = (self.to_string(), other.to_string()) else {
                return false;
            };
            return left == right;
        }
        match (self, other) {
            (Self::Void, Self::Integer(value)) | (Self::Integer(value), Self::Void) => *value == 0,
            (Self::Void, Self::Real(value)) | (Self::Real(value), Self::Void) => *value == 0.0,
            (Self::Integer(left), Self::Real(right)) | (Self::Real(right), Self::Integer(left)) => {
                *left as f64 == *right
            }
            _ => false,
        }
    }

    /// The order of the two values for the `<` and `>` operators.
    ///
    /// Two texts compare by code unit and two integers by value. Every other
    /// comparable pair compares as a real, so a text beside a number is read
    /// as a number. `None` is an unordered pair: one operand is a
    /// not-a-number, and both `<` and `>` answer false for it. An octet and
    /// an object are not ordered at all.
    pub fn compare(&self, other: &Self) -> Result<Option<Ordering>, VariantError> {
        if let Self::String(left) = self
            && let Self::String(right) = other
        {
            return Ok(Some(left.cmp(right)));
        }
        if let Self::Integer(left) = self
            && let Self::Integer(right) = other
        {
            return Ok(Some(left.cmp(right)));
        }
        let left = self.to_real()?;
        let right = other.to_real()?;
        Ok(left.partial_cmp(&right))
    }
}

/// The integer a number converts to, with the language's truncation.
fn integer_of(number: Number) -> Result<i64, VariantError> {
    match number.to_integer() {
        Ok(integer) => Ok(integer),
        Err(NumberError::RealOutOfRange) => Err(VariantError::IntegerOutOfRange),
        Err(NumberError::NotANumber) => Err(VariantError::Convert {
            to: Target::Integer.name(),
        }),
    }
}

/// The kind a refused conversion names as its target.
///
/// The words are the engine's: `int`, `real`, `string`, `octet`, `int/real`
/// and `Object`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// An integer.
    Integer,
    /// A real number.
    Real,
    /// A text.
    Text,
    /// A byte string.
    Octet,
    /// A number: an integer or a real.
    Number,
    /// An object.
    Object,
}

impl Target {
    /// The word the engine's message names the kind with.
    pub fn name(self) -> &'static str {
        match self {
            Self::Integer => "int",
            Self::Real => "real",
            Self::Text => "string",
            Self::Octet => "octet",
            Self::Number => "int/real",
            Self::Object => "Object",
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// The message of a division by zero.
pub(crate) const DIVISION_BY_ZERO_MESSAGE: &str = "Division by zero";

/// The greatest number of code units the readable form of a value keeps.
const READABLE_LENGTH: usize = 512;

/// Cuts a readable form to the engine's length, with an ellipsis.
pub(crate) fn trim_readable(text: TjsString) -> TjsString {
    if text.len() <= READABLE_LENGTH {
        return text;
    }
    let mut units: Vec<u16> = text.as_utf16()[..READABLE_LENGTH - 3].to_vec();
    units.extend("...".encode_utf16());
    TjsString::from(units)
}

/// The message a refused conversion raises.
///
/// The value is already in its readable form, and the target is the kind the
/// conversion asked for.
pub fn conversion_message(value: &str, target: &str) -> String {
    format!("Cannot convert the variable type ({value} to {target})")
}

/// The shift count the hardware uses: the low six bits of the count.
fn shift_count(count: i64) -> u32 {
    (count & 63) as u32
}

/// The failure of a conversion or of an operator.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum VariantError {
    /// The value has no representation of the kind a conversion asked for.
    #[error("the value cannot be converted to {to}")]
    Convert {
        /// The kind the conversion asked for, in the target word.
        to: &'static str,
    },
    /// A division or a remainder has a divisor of zero.
    #[error("division by zero")]
    DivideByZero,
    /// An integer result does not fit a 64-bit integer.
    #[error("the integer result is out of range")]
    IntegerOutOfRange,
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::rc::Rc;

    use super::{Closure, Dispatch, Target, Variant, VariantError, conversion_message};
    use crate::vm::runtime::number::Number;
    use crate::vm::runtime::object::{MemberFlags, ObjectError};
    use crate::vm::runtime::octet::Octet;
    use crate::vm::runtime::string::TjsString;

    /// A stand-in for an object of the object model.
    struct TestObject;

    impl Dispatch for TestObject {
        fn prop_get(
            &self,
            _name: &TjsString,
            _flags: MemberFlags,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<Variant, ObjectError> {
            Err(ObjectError::MemberNotFound)
        }

        fn prop_set(
            &self,
            _name: &TjsString,
            _flags: MemberFlags,
            _value: &Variant,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<(), ObjectError> {
            Err(ObjectError::MemberNotFound)
        }

        fn delete_member(
            &self,
            _name: &TjsString,
            _flags: MemberFlags,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<(), ObjectError> {
            Err(ObjectError::MemberNotFound)
        }

        fn enum_members(
            &self,
            _flags: MemberFlags,
            _callback: &mut dyn FnMut(&TjsString, u32, &Variant) -> bool,
        ) -> Result<(), ObjectError> {
            Ok(())
        }

        fn is_instance_of(
            &self,
            _class: &TjsString,
            _objthis: Option<&dyn Dispatch>,
        ) -> Result<bool, ObjectError> {
            Ok(false)
        }

        fn class_names(&self) -> Vec<TjsString> {
            Vec::new()
        }

        fn is_valid(&self) -> bool {
            true
        }

        fn invalidate(&self) -> bool {
            false
        }

        fn add_class_name(&self, _name: &TjsString) -> Result<(), ObjectError> {
            Ok(())
        }
    }

    /// The object value over a dispatch.
    fn object_of(dispatch: &Rc<dyn Dispatch>) -> Variant {
        Variant::Object(Closure {
            object: Some(Rc::clone(dispatch)),
            objthis: None,
        })
    }

    /// The answer of an operator as `kind: text`, so one comparison checks
    /// the kind and the text together.
    fn answered(value: Result<Variant, VariantError>) -> String {
        let value = value.expect("the operator does not raise here");
        let text = value.to_string().expect("the value has a text");
        format!("{}: {text}", value.type_name())
    }

    /// Whether the text is the reference's object form.
    fn is_object_form(text: &TjsString) -> bool {
        let text = text.to_string();
        text.starts_with("(object ") && text.ends_with(')') && text.contains(':')
    }

    // reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638
    #[test]
    fn the_kind_is_the_typeof_spelling() {
        assert_eq!(Variant::Void.type_name(), "void");
        assert_eq!(Variant::Integer(1).type_name(), "Integer");
        assert_eq!(Variant::Real(1.5).type_name(), "Real");
        assert_eq!(Variant::from("a").type_name(), "String");
        assert_eq!(Variant::from(Octet::new()).type_name(), "Octet");
        assert_eq!(Variant::Object(Closure::null()).type_name(), "Object");
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:213-249
    #[test]
    fn a_number_writes_its_text() {
        assert_eq!(Variant::Integer(-5).to_string(), Ok(TjsString::from("-5")));
        assert_eq!(Variant::Real(1.5).to_string(), Ok(TjsString::from("1.5")));
        assert_eq!(Variant::Real(0.0).to_string(), Ok(TjsString::from("+0.0")));
    }

    // reference/cpp/core/tjs2/tjsVariant.h:755-775
    // reference/cpp/core/tjs2/tjsVariant.h:862-879
    // reference/cpp/core/tjs2/tjsVariant.h:937-955
    #[test]
    fn the_empty_variant_converts_to_the_empty_text_and_zero() {
        assert_eq!(Variant::Void.to_string(), Ok(TjsString::new()));
        assert_eq!(Variant::Void.to_number(), Ok(Number::Integer(0)));
        assert_eq!(Variant::Void.to_integer(), Ok(0));
        assert_eq!(Variant::Void.to_real(), Ok(0.0));
    }

    // reference/cpp/core/tjs2/tjsVariant.h:881-903
    // reference/cpp/core/tjs2/tjsVariantString.cpp:481-493
    #[test]
    fn a_text_is_read_with_the_languages_number_rules() {
        assert_eq!(Variant::from("12.5").to_number(), Ok(Number::Real(12.5)));
        assert_eq!(Variant::from("12").to_number(), Ok(Number::Integer(12)));
        assert_eq!(Variant::from("12").to_integer(), Ok(12));
        assert_eq!(Variant::from("12.5").to_integer(), Ok(12));
        assert_eq!(Variant::from("12").to_real(), Ok(12.0));
        assert_eq!(Variant::from("1e3").to_real(), Ok(1000.0));
    }

    /// A text that is not a number is the integer zero, not a real zero and
    /// not an error.
    ///
    /// reference/cpp/core/tjs2/tjsVariantString.cpp:481-493
    #[test]
    fn a_text_that_is_not_a_number_is_the_integer_zero() {
        let word = Variant::from("abc");
        assert_eq!(word.to_number(), Ok(Number::Integer(0)));
        assert_eq!(word.to_integer(), Ok(0));
        assert_eq!(word.to_real(), Ok(0.0));
        assert!(matches!(word.to_number(), Ok(Number::Integer(0))));
    }

    // reference/cpp/core/tjs2/tjsVariant.h:810-841
    // reference/cpp/core/tjs2/tjsVariant.cpp:441-458
    #[test]
    fn an_octet_converts_only_to_an_octet() {
        let octet = Octet::from_slice(&[1, 2]);
        let value = Variant::from(octet.clone());
        assert_eq!(value.to_octet(), Ok(octet));
        assert_eq!(Variant::Void.to_octet(), Ok(Octet::new()));
        assert!(Variant::Integer(1).to_octet().is_err());
        assert!(Variant::from("a").to_octet().is_err());
        assert_eq!(
            value.to_string(),
            Err(VariantError::Convert {
                to: Target::Text.name()
            })
        );
        assert!(value.to_number().is_err());
        assert_eq!(
            value.to_integer(),
            Err(VariantError::Convert {
                to: Target::Integer.name()
            })
        );
        assert_eq!(
            value.to_real(),
            Err(VariantError::Convert {
                to: Target::Real.name()
            })
        );
    }

    /// The readable form is the engine's: the kind in parentheses and the
    /// value after it, trimmed to 512 units with an ellipsis, and the null
    /// object's form is the deterministic `(object 0x0:0x0)`.
    ///
    /// reference/cpp/core/tjs2/tjsUtils.cpp:78-90
    /// reference/cpp/core/tjs2/tjsUtils.cpp:111-173
    #[test]
    fn the_readable_form_is_the_engines() {
        assert_eq!(Variant::Void.readable(), TjsString::from("()"));
        assert_eq!(Variant::Integer(1).readable(), TjsString::from("(int)1"));
        assert_eq!(Variant::Real(1.5).readable(), TjsString::from("(real)1.5"));
        assert_eq!(
            Variant::from("a\n").readable(),
            TjsString::from("(string)\"a\\n\"")
        );
        assert_eq!(
            Variant::from(Octet::from_slice(&[1])).readable(),
            TjsString::from("(octet)<% 01 %>")
        );
        assert_eq!(
            Variant::Object(Closure::null()).readable(),
            TjsString::from("(object)(object 0x0:0x0)")
        );

        let long = Variant::from(Octet::from_slice(&[1; 174])).readable();
        let expected = format!("(octet)<% {}0...", "01 ".repeat(166));
        assert_eq!(long, TjsString::from(expected));
        assert_eq!(long.len(), 512);
    }

    /// A refused conversion's message quotes the value and the target word,
    /// and the target `Object` keeps its capital.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.cpp:146-163
    /// reference/cpp/core/tjs2/tjsError.h:250-253
    #[test]
    fn a_conversion_message_names_the_value_and_the_target() {
        let null = Variant::Object(Closure::null());
        assert_eq!(
            conversion_message(&null.readable().to_string(), Target::Real.name()),
            "Cannot convert the variable type ((object)(object 0x0:0x0) to real)"
        );
        assert_eq!(
            conversion_message(&null.readable().to_string(), Target::Object.name()),
            "Cannot convert the variable type ((object)(object 0x0:0x0) to Object)"
        );
        assert_eq!(
            conversion_message(
                &Variant::from(Octet::from_slice(&[1]))
                    .readable()
                    .to_string(),
                Target::Text.name()
            ),
            "Cannot convert the variable type ((octet)<% 01 %> to string)"
        );
    }

    /// An object's text is its form: the two handles it carries, and `0x0`
    /// for one it does not.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.h:755-759
    /// reference/cpp/core/tjs2/tjsVariant.cpp:187-210
    #[test]
    fn an_objects_text_is_its_form() {
        let dispatch: Rc<dyn Dispatch> = Rc::new(TestObject);
        let value = object_of(&dispatch);
        let text = value.to_string().expect("an object has its form");
        assert!(is_object_form(&text), "not the object form: {text}");
        // the null object carries no handle, so its form holds two zeros
        let null = Variant::Object(Closure::null());
        assert_eq!(null.type_name(), "Object");
        assert_eq!(null.to_string(), Ok(TjsString::from("(object 0x0:0x0)")));
        assert!(Closure::null().is_null());
    }

    /// An object has no number: every numeric coercion raises.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.h:866-867
    /// reference/cpp/core/tjs2/tjsVariant.h:886-887
    /// reference/cpp/core/tjs2/tjsVariant.h:941-942
    #[test]
    fn an_object_has_no_number() {
        let dispatch: Rc<dyn Dispatch> = Rc::new(TestObject);
        let value = object_of(&dispatch);
        assert_eq!(
            value.to_number(),
            Err(VariantError::Convert {
                to: Target::Number.name()
            })
        );
        assert_eq!(
            value.to_integer(),
            Err(VariantError::Convert {
                to: Target::Integer.name()
            })
        );
        assert_eq!(
            value.to_real(),
            Err(VariantError::Convert {
                to: Target::Real.name()
            })
        );
        let null = Variant::Object(Closure::null());
        assert!(null.to_number().is_err());
        assert!(null.to_integer().is_err());
        assert!(null.to_real().is_err());
        // and a null object is not the empty variant
        assert!(!null.equals(&Variant::Void));
        assert!(null.equals(&Variant::Object(Closure::null())));
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:1123-1202
    #[test]
    fn a_text_operand_concatenates() {
        assert_eq!(
            answered(Variant::from("a").add(&Variant::Integer(1))),
            "String: a1"
        );
        assert_eq!(
            answered(Variant::Integer(1).add(&Variant::from("a"))),
            "String: 1a"
        );
        assert_eq!(
            answered(Variant::from("5").add(&Variant::from("2"))),
            "String: 52"
        );
        assert_eq!(
            answered(Variant::from("a").add(&Variant::Void)),
            "String: a"
        );
        assert_eq!(
            answered(Variant::Real(1.5).add(&Variant::from("x"))),
            "String: 1.5x"
        );
        assert_eq!(
            answered(Variant::Void.add(&Variant::from("x"))),
            "String: x"
        );
        assert!(
            Variant::from("a")
                .add(&Variant::from(Octet::new()))
                .is_err()
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:1123-1202
    #[test]
    fn two_octets_join_and_an_octet_adds_to_no_number() {
        let joined = Variant::from(Octet::from_slice(&[1]))
            .add(&Variant::from(Octet::from_two(&[1], &[2])))
            .expect("an octet pairs with an octet");
        match joined {
            Variant::Octet(octet) => assert_eq!(octet.as_slice(), &[1, 1, 2]),
            other => panic!("the join is not an octet: {other:?}"),
        }
        assert!(
            Variant::from(Octet::new())
                .add(&Variant::Integer(1))
                .is_err()
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:1106-1121
    // reference/cpp/core/tjs2/tjsVariant.cpp:1040-1055
    #[test]
    fn integer_arithmetic_wraps() {
        assert_eq!(
            answered(Variant::Integer(1).subtract(&Variant::Integer(2))),
            "Integer: -1"
        );
        assert_eq!(
            answered(Variant::Integer(3).multiply(&Variant::Integer(4))),
            "Integer: 12"
        );
        assert_eq!(
            answered(Variant::Integer(i64::MAX).add(&Variant::Integer(1))),
            "Integer: -9223372036854775808"
        );
        assert_eq!(
            answered(Variant::from("5").multiply(&Variant::from("2"))),
            "Integer: 10"
        );
        assert_eq!(
            answered(Variant::from("5").subtract(&Variant::Integer(2))),
            "Integer: 3"
        );
        assert_eq!(
            answered(Variant::Integer(1).add(&Variant::Real(0.5))),
            "Real: 1.5"
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:1019-1027
    // reference/cpp/core/tjs2/tjsVariant.cpp:1029-1038
    // reference/cpp/core/tjs2/tjsVariant.cpp:1008-1017
    #[test]
    fn division_is_real_and_the_integer_forms_raise_on_zero() {
        assert_eq!(
            answered(Variant::Integer(7).divide(&Variant::Integer(2))),
            "Real: 3.5"
        );
        assert_eq!(
            answered(Variant::Integer(6).divide(&Variant::Integer(3))),
            "Real: 2"
        );
        assert_eq!(
            answered(Variant::Integer(7).idivide(&Variant::Integer(2))),
            "Integer: 3"
        );
        assert_eq!(
            answered(Variant::Integer(7).remainder(&Variant::Integer(3))),
            "Integer: 1"
        );
        assert_eq!(
            answered(Variant::Integer(-7).remainder(&Variant::Integer(3))),
            "Integer: -1"
        );
        assert!(matches!(
            Variant::Integer(1).idivide(&Variant::Integer(0)),
            Err(VariantError::DivideByZero)
        ));
        assert!(matches!(
            Variant::Integer(1).remainder(&Variant::Integer(0)),
            Err(VariantError::DivideByZero)
        ));
        assert!(matches!(
            Variant::Integer(i64::MIN).idivide(&Variant::Integer(-1)),
            Err(VariantError::IntegerOutOfRange)
        ));
        assert!(matches!(
            Variant::Real(1.0).divide(&Variant::Real(0.0)),
            Ok(Variant::Real(value)) if value.is_infinite()
        ));
    }

    /// An empty variant beside a number keeps the number's kind, and two
    /// empty variants add as a real.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.cpp:1123-1202
    #[test]
    fn a_void_operand_keeps_the_other_kind() {
        assert_eq!(
            answered(Variant::Void.add(&Variant::Integer(1))),
            "Integer: 1"
        );
        assert_eq!(
            answered(Variant::Integer(1).add(&Variant::Void)),
            "Integer: 1"
        );
        assert_eq!(
            answered(Variant::Void.add(&Variant::Real(1.5))),
            "Real: 1.5"
        );
        assert_eq!(
            answered(Variant::Real(1.5).add(&Variant::Void)),
            "Real: 1.5"
        );
        assert_eq!(answered(Variant::Void.add(&Variant::Void)), "Real: +0.0");
        assert_eq!(
            answered(Variant::Void.subtract(&Variant::Integer(1))),
            "Integer: -1"
        );
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:928-934
    // reference/cpp/core/tjs2/tjsVariant.cpp:968-1006
    #[test]
    fn the_bitwise_family_is_integer() {
        assert_eq!(
            answered(Variant::Integer(6).bit_and(&Variant::Integer(3))),
            "Integer: 2"
        );
        assert_eq!(
            answered(Variant::Integer(6).bit_or(&Variant::Integer(3))),
            "Integer: 7"
        );
        assert_eq!(
            answered(Variant::Integer(6).bit_xor(&Variant::Integer(3))),
            "Integer: 5"
        );
        assert_eq!(
            answered(Variant::from("6").bit_and(&Variant::Integer(3))),
            "Integer: 2"
        );
        assert_eq!(
            answered(Variant::Integer(1).shift_left(&Variant::Integer(4))),
            "Integer: 16"
        );
        assert_eq!(
            answered(Variant::Integer(-8).shift_right(&Variant::Integer(1))),
            "Integer: -4"
        );
        // the count is the hardware's: the low six bits
        assert_eq!(
            answered(Variant::Integer(1).shift_left(&Variant::Integer(64))),
            "Integer: 1"
        );
        assert_eq!(answered(Variant::Integer(5).complement()), "Integer: -6");
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:936-966
    #[test]
    fn increment_and_decrement() {
        assert_eq!(answered(Variant::Void.increment()), "Integer: 1");
        assert_eq!(answered(Variant::Void.decrement()), "Integer: -1");
        assert_eq!(answered(Variant::from("5").increment()), "Integer: 6");
        assert_eq!(answered(Variant::from("1.5").decrement()), "Real: 0.5");
        assert_eq!(answered(Variant::Real(1.5).increment()), "Real: 2.5");
        for answer in [
            Variant::from(Octet::new()).increment(),
            Variant::from(Octet::new()).decrement(),
        ] {
            assert!(matches!(
                answer,
                Err(VariantError::Convert { to }) if to == Target::Number.name()
            ));
        }
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:1057-1071
    // reference/cpp/core/tjs2/tjsVariant.cpp:1091-1104
    #[test]
    fn negation_and_the_complement() {
        assert_eq!(answered(Variant::Integer(5).negate()), "Integer: -5");
        assert_eq!(answered(Variant::Void.negate()), "Integer: 0");
        assert_eq!(answered(Variant::from("5").negate()), "Integer: -5");
        assert_eq!(answered(Variant::from("abc").negate()), "Integer: 0");
        assert_eq!(answered(Variant::Real(1.5).negate()), "Real: -1.5");
        assert_eq!(answered(Variant::Integer(-6).complement()), "Integer: 5");
        assert!(Variant::from(Octet::new()).negate().is_err());
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:665-804
    #[test]
    fn equals_follows_the_reference_order() {
        assert!(Variant::Integer(1).equals(&Variant::Integer(1)));
        assert!(Variant::Integer(1).equals(&Variant::Real(1.0)));
        assert!(!Variant::Integer(1).equals(&Variant::Real(1.5)));
        // a text operand compares as text
        assert!(Variant::from("10").equals(&Variant::Integer(10)));
        assert!(!Variant::from("10").equals(&Variant::Integer(9)));
        assert!(!Variant::from("a").equals(&Variant::Integer(0)));
        assert!(Variant::from("a").equals(&Variant::from("a")));
        assert!(!Variant::from("a").equals(&Variant::from("A")));
        // the empty variant equals the integer zero and the empty text
        assert!(Variant::Void.equals(&Variant::Integer(0)));
        assert!(Variant::Void.equals(&Variant::Real(0.0)));
        assert!(Variant::Void.equals(&Variant::from("")));
        assert!(!Variant::Void.equals(&Variant::Integer(1)));
        assert!(!Variant::Void.equals(&Variant::from(Octet::new())));
        assert!(!Variant::Void.equals(&Variant::Object(Closure::null())));
        // an octet equals only an octet
        assert!(
            Variant::from(Octet::from_slice(&[1])).equals(&Variant::from(Octet::from_slice(&[1])))
        );
        assert!(!Variant::from(Octet::from_slice(&[1])).equals(&Variant::Integer(1)));
        // a not-a-number is equal to nothing
        assert!(!Variant::Real(f64::NAN).equals(&Variant::Real(f64::NAN)));
        assert!(Variant::Real(f64::INFINITY).equals(&Variant::Real(f64::INFINITY)));
        assert!(!Variant::Real(f64::INFINITY).equals(&Variant::Real(f64::NEG_INFINITY)));
    }

    // reference/cpp/core/tjs2/tjsVariant.cpp:856-910
    #[test]
    fn the_order_of_a_text_and_a_number_is_numeric() {
        assert_eq!(
            Variant::from("10").compare(&Variant::Integer(9)),
            Ok(Some(Ordering::Greater))
        );
        assert_eq!(
            Variant::from("10").compare(&Variant::Integer(10)),
            Ok(Some(Ordering::Equal))
        );
        assert_eq!(
            Variant::from("abc").compare(&Variant::Integer(1)),
            Ok(Some(Ordering::Less))
        );
        // two texts compare by code unit
        assert_eq!(
            Variant::from("abc").compare(&Variant::from("abd")),
            Ok(Some(Ordering::Less))
        );
        assert_eq!(
            Variant::from("a").compare(&Variant::from("A")),
            Ok(Some(Ordering::Greater))
        );
        // two integers compare by value, mixed kinds as reals
        assert_eq!(
            Variant::Integer(2).compare(&Variant::Integer(10)),
            Ok(Some(Ordering::Less))
        );
        assert_eq!(
            Variant::Integer(2).compare(&Variant::Real(2.5)),
            Ok(Some(Ordering::Less))
        );
        // a not-a-number orders nothing
        assert_eq!(
            Variant::Real(f64::NAN).compare(&Variant::Real(1.0)),
            Ok(None)
        );
        // an octet and an object are not ordered
        assert!(
            Variant::from(Octet::new())
                .compare(&Variant::Integer(1))
                .is_err()
        );
        assert!(
            Variant::Object(Closure::null())
                .compare(&Variant::Integer(1))
                .is_err()
        );
    }

    /// Two object values are equal when they are the same object.
    ///
    /// reference/cpp/core/tjs2/tjsVariant.cpp:733-745
    #[test]
    fn two_objects_compare_by_identity() {
        let first: Rc<dyn Dispatch> = Rc::new(TestObject);
        let second: Rc<dyn Dispatch> = Rc::new(TestObject);
        let value = object_of(&first);
        assert!(value.equals(&object_of(&first)));
        assert!(!value.equals(&object_of(&second)));
    }
}
