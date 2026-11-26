//! The `Math` class's members: the arithmetic functions and the constants.
//!
//! Every function reads each argument as a real. An integer widens to a real;
//! a text is read by the language's number rules, so a text that is not a
//! number reads as zero; the empty value reads as zero; an object and an octet
//! raise. A function with a required parameter raises when the parameter is
//! missing, and a parameter past the required ones is ignored.
//!
//! `max` and `min` take any number of parameters, including none, and answer
//! the negative or the positive infinity then. A not-a-number among the
//! parameters answers the canonical not-a-number, and the two zeroes keep
//! their sign: `max(-0.0, 0.0)` is the positive zero and `min(0.0, -0.0)` is
//! the negative one.
//!
//! `round` is `floor(value + 0.5)`, so a half rounds toward the positive
//! infinity and `round(-0.5)` is the positive zero.
//!
//! The generator-backed `random` member is not here: its state belongs to the
//! class object, so the part that owns the class supplies it. The constants
//! are read properties, not calls.

use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Variant, VariantError};

/// A member of the class.
///
/// The members are the functions the class publishes and the constants it
/// publishes; [`MathMember::ALL`] lists them in the order the class registers
/// them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MathMember {
    /// The absolute value.
    Abs,
    /// The arc cosine.
    Acos,
    /// The arc sine.
    Asin,
    /// The arc tangent.
    Atan,
    /// The arc tangent of two values.
    Atan2,
    /// The smallest integer not below the value.
    Ceil,
    /// The cosine.
    Cos,
    /// The exponential.
    Exp,
    /// The greatest integer not above the value.
    Floor,
    /// The natural logarithm.
    Log,
    /// The greatest parameter.
    Max,
    /// The smallest parameter.
    Min,
    /// The value raised to a power.
    Pow,
    /// The nearest integer, with a half toward the positive infinity.
    Round,
    /// The sine.
    Sin,
    /// The square root.
    Sqrt,
    /// The tangent.
    Tan,
    /// The base of the natural logarithm.
    E,
    /// The base-two logarithm of `E`.
    Log2E,
    /// The base-ten logarithm of `E`.
    Log10E,
    /// The natural logarithm of ten.
    Ln10,
    /// The natural logarithm of two.
    Ln2,
    /// The ratio of a circle's circumference to its diameter.
    Pi,
    /// The square root of one half.
    Sqrt1_2,
    /// The square root of two.
    Sqrt2,
}

impl MathMember {
    /// Every member, in the order the class registers them.
    pub const ALL: &'static [MathMember] = &[
        Self::Abs,
        Self::Acos,
        Self::Asin,
        Self::Atan,
        Self::Atan2,
        Self::Ceil,
        Self::Cos,
        Self::Exp,
        Self::Floor,
        Self::Log,
        Self::Max,
        Self::Min,
        Self::Pow,
        Self::Round,
        Self::Sin,
        Self::Sqrt,
        Self::Tan,
        Self::E,
        Self::Log2E,
        Self::Log10E,
        Self::Ln10,
        Self::Ln2,
        Self::Pi,
        Self::Sqrt1_2,
        Self::Sqrt2,
    ];

    /// The member `name` denotes, or `None` when it names no member.
    pub fn lookup(name: &TjsString) -> Option<Self> {
        let units = name.as_utf16();
        Self::ALL
            .iter()
            .copied()
            .find(|member| units.iter().copied().eq(member.name().encode_utf16()))
    }

    /// The name the class publishes the member under.
    pub fn name(self) -> &'static str {
        match self {
            Self::Abs => "abs",
            Self::Acos => "acos",
            Self::Asin => "asin",
            Self::Atan => "atan",
            Self::Atan2 => "atan2",
            Self::Ceil => "ceil",
            Self::Cos => "cos",
            Self::Exp => "exp",
            Self::Floor => "floor",
            Self::Log => "log",
            Self::Max => "max",
            Self::Min => "min",
            Self::Pow => "pow",
            Self::Round => "round",
            Self::Sin => "sin",
            Self::Sqrt => "sqrt",
            Self::Tan => "tan",
            Self::E => "E",
            Self::Log2E => "LOG2E",
            Self::Log10E => "LOG10E",
            Self::Ln10 => "LN10",
            Self::Ln2 => "LN2",
            Self::Pi => "PI",
            Self::Sqrt1_2 => "SQRT1_2",
            Self::Sqrt2 => "SQRT2",
        }
    }

    /// The fewest parameters a call accepts, or `None` for a constant.
    pub fn min_params(self) -> Option<usize> {
        match self {
            Self::Abs
            | Self::Acos
            | Self::Asin
            | Self::Atan
            | Self::Ceil
            | Self::Cos
            | Self::Exp
            | Self::Floor
            | Self::Log
            | Self::Round
            | Self::Sin
            | Self::Sqrt
            | Self::Tan => Some(1),
            Self::Atan2 | Self::Pow => Some(2),
            Self::Max | Self::Min => Some(0),
            Self::E
            | Self::Log2E
            | Self::Log10E
            | Self::Ln10
            | Self::Ln2
            | Self::Pi
            | Self::Sqrt1_2
            | Self::Sqrt2 => None,
        }
    }

    /// The value of a constant, or `None` for a function.
    pub fn constant(self) -> Option<f64> {
        match self {
            Self::E => Some(std::f64::consts::E),
            Self::Log2E => Some(std::f64::consts::LOG2_E),
            Self::Log10E => Some(std::f64::consts::LOG10_E),
            Self::Ln10 => Some(std::f64::consts::LN_10),
            Self::Ln2 => Some(std::f64::consts::LN_2),
            Self::Pi => Some(std::f64::consts::PI),
            Self::Sqrt1_2 => Some(std::f64::consts::SQRT_2 / 2.0),
            Self::Sqrt2 => Some(std::f64::consts::SQRT_2),
            Self::Abs
            | Self::Acos
            | Self::Asin
            | Self::Atan
            | Self::Atan2
            | Self::Ceil
            | Self::Cos
            | Self::Exp
            | Self::Floor
            | Self::Log
            | Self::Max
            | Self::Min
            | Self::Pow
            | Self::Round
            | Self::Sin
            | Self::Sqrt
            | Self::Tan => None,
        }
    }

    /// The value of a constant, as the value a read answers.
    pub fn value(self) -> Option<Variant> {
        self.constant().map(Variant::Real)
    }

    /// Calls the member with the given arguments.
    ///
    /// A function reads the parameters it requires as reals and ignores the
    /// rest. A constant is not a call.
    ///
    /// # Errors
    ///
    /// [`MathError::ParameterCount`] when a required parameter is missing,
    /// [`MathError::Convert`] when a parameter has no real, and
    /// [`MathError::NotCallable`] when the member is a constant.
    pub fn call(self, arguments: &[Variant]) -> Result<Variant, MathError> {
        let Some(required) = self.min_params() else {
            return Err(MathError::NotCallable);
        };
        if arguments.len() < required {
            return Err(MathError::ParameterCount {
                required,
                actual: arguments.len(),
            });
        }
        match self {
            Self::Abs => unary(arguments, required, f64::abs),
            Self::Acos => unary(arguments, required, f64::acos),
            Self::Asin => unary(arguments, required, f64::asin),
            Self::Atan => unary(arguments, required, f64::atan),
            Self::Atan2 => binary(arguments, required, f64::atan2),
            Self::Ceil => unary(arguments, required, f64::ceil),
            Self::Cos => unary(arguments, required, f64::cos),
            Self::Exp => unary(arguments, required, f64::exp),
            Self::Floor => unary(arguments, required, f64::floor),
            Self::Log => unary(arguments, required, f64::ln),
            Self::Max => maximum(arguments),
            Self::Min => minimum(arguments),
            Self::Pow => binary(arguments, required, f64::powf),
            Self::Round => {
                let value = argument(arguments, 0, required)?.to_real()?;
                Ok(Variant::Real((value + 0.5).floor()))
            }
            Self::Sin => unary(arguments, required, f64::sin),
            Self::Sqrt => unary(arguments, required, f64::sqrt),
            Self::Tan => unary(arguments, required, f64::tan),
            Self::E
            | Self::Log2E
            | Self::Log10E
            | Self::Ln10
            | Self::Ln2
            | Self::Pi
            | Self::Sqrt1_2
            | Self::Sqrt2 => Err(MathError::NotCallable),
        }
    }
}

/// The failure of a call.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum MathError {
    /// The call gives fewer parameters than the member requires.
    #[error("the member needs at least {required} parameter(s), but {actual} were given")]
    ParameterCount {
        /// The fewest parameters the member accepts.
        required: usize,
        /// The number of parameters the call gives.
        actual: usize,
    },

    /// The member is a constant, which is read rather than called.
    #[error("the constant is not callable")]
    NotCallable,

    /// A parameter has no real.
    #[error("the value cannot be converted")]
    Convert(#[from] VariantError),
}

/// Applies a one-parameter function to the first argument.
fn unary(
    arguments: &[Variant],
    required: usize,
    operation: fn(f64) -> f64,
) -> Result<Variant, MathError> {
    let value = argument(arguments, 0, required)?.to_real()?;
    Ok(Variant::Real(operation(value)))
}

/// Applies a two-parameter function to the first two arguments.
fn binary(
    arguments: &[Variant],
    required: usize,
    operation: fn(f64, f64) -> f64,
) -> Result<Variant, MathError> {
    let left = argument(arguments, 0, required)?.to_real()?;
    let right = argument(arguments, 1, required)?.to_real()?;
    Ok(Variant::Real(operation(left, right)))
}

/// The argument at `index`, or the count failure the member's arity names.
fn argument(arguments: &[Variant], index: usize, required: usize) -> Result<&Variant, MathError> {
    arguments.get(index).ok_or(MathError::ParameterCount {
        required,
        actual: arguments.len(),
    })
}

/// The greatest parameter, or the negative infinity for none.
fn maximum(arguments: &[Variant]) -> Result<Variant, MathError> {
    let mut greatest = f64::NEG_INFINITY;
    for argument in arguments {
        let value = argument.to_real()?;
        if value.is_nan() {
            return Ok(Variant::Real(f64::NAN));
        }
        // a strict comparison, so a tie keeps the argument that came first -
        // which is what decides the sign of a zero
        if greatest < value {
            greatest = value;
        }
    }
    Ok(Variant::Real(greatest))
}

/// The smallest parameter, or the positive infinity for none.
fn minimum(arguments: &[Variant]) -> Result<Variant, MathError> {
    let mut smallest = f64::INFINITY;
    for argument in arguments {
        let value = argument.to_real()?;
        if value.is_nan() {
            return Ok(Variant::Real(f64::NAN));
        }
        // a strict comparison, so a tie keeps the argument that came first
        if value < smallest {
            smallest = value;
        }
    }
    Ok(Variant::Real(smallest))
}
