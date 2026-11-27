//! The operators the VM applies: the variant's own rules, and the four the
//! variant does not carry.
//!
//! The language's arithmetic is the variant's, so most of an instruction is one
//! call: `+=` is [`Variant::add`], `>>=` is [`Variant::shift_right`], `~` is
//! [`Variant::complement`]. Four rules are not the variant's, because they do
//! not belong to a value in isolation:
//!
//! | rule | what it is |
//! |---|---|
//! | [`truth`] | the truth of a value, which an object answers by having one and a text by reading as a number |
//! | [`discern_equals`] | the strict comparison, which compares an object by *both* of its handles and holds no kind equal to another |
//! | [`logical_shift_right`] | `>>>`, an unsigned shift, where the variant's own shift is arithmetic |
//! | [`character_code`], [`character_from`] | `#` and `$`, the code of a text's first unit and the one-unit text of a code |
//!
//! # The failures
//!
//! A conversion the language refuses, a divisor of zero and an integer result
//! that does not fit are the three ways an instruction fails. Each becomes a
//! [`Raise`] carrying the value that failed and, for a conversion, the kind it
//! asked for. The text a script reads is built where the raise becomes a script
//! exception, not here, because that is where the trace and the exception
//! object are made.
//!
//! Which operand a conversion failure names follows the engine's own order:
//! every operator converts its left operand first, except the remainder and the
//! integer division, which read the divisor first.

use std::cmp::Ordering;
use std::rc::Rc;

use super::Raise;
use super::decode::Operator;
use super::throw::Unwind;
use crate::vm::runtime::number::Number;
use crate::vm::runtime::object::Dispatch;
use crate::vm::runtime::string::TjsString;
use crate::vm::runtime::variant::{Variant, VariantError};

/// The value of `left op right`, with `op` naming the operator.
///
/// Every rule here is the language's; a pair the language has no operation for
/// is a [`Raise`], never a fabricated value.
pub fn apply(operator: Operator, left: &Variant, right: &Variant) -> Result<Variant, Unwind> {
    let compound = |result: Result<Variant, VariantError>| -> Result<Variant, Unwind> {
        result.map_err(|error| raised(error, left, right, operator))
    };
    match operator {
        Operator::LOr => {
            let value = truth(left)? || truth(right)?;
            Ok(Variant::Integer(i64::from(value)))
        }
        Operator::LAnd => {
            let value = truth(left)? && truth(right)?;
            Ok(Variant::Integer(i64::from(value)))
        }
        Operator::Sr => logical_shift_right(left, right),
        Operator::Add => compound(left.add(right)),
        Operator::Sub => compound(left.subtract(right)),
        Operator::Mul => compound(left.multiply(right)),
        Operator::Div => compound(left.divide(right)),
        Operator::IDiv => compound(left.idivide(right)),
        Operator::Mod => compound(left.remainder(right)),
        Operator::BOr => compound(left.bit_or(right)),
        Operator::BXor => compound(left.bit_xor(right)),
        Operator::BAnd => compound(left.bit_and(right)),
        Operator::Sar => compound(left.shift_right(right)),
        Operator::Sal => compound(left.shift_left(right)),
    }
}

/// The value of `self + 1`.
///
/// The step instructions convert their operand the way the arithmetic does, and
/// an octet or an object has no increment.
pub fn step_up(value: &Variant) -> Result<Variant, Unwind> {
    value
        .increment()
        .map_err(|error| refused_conversion(error, value))
}

/// The value of `self - 1`.
pub fn step_down(value: &Variant) -> Result<Variant, Unwind> {
    value
        .decrement()
        .map_err(|error| refused_conversion(error, value))
}

/// The value of `~self`.
pub fn complement(value: &Variant) -> Result<Variant, Unwind> {
    value
        .complement()
        .map_err(|error| refused_conversion(error, value))
}

/// The value of `-self`.
pub fn negate(value: &Variant) -> Result<Variant, Unwind> {
    value
        .negate()
        .map_err(|error| refused_conversion(error, value))
}

/// The value of `+self`: the number the value reads as, or the value itself when
/// it already is one.
///
/// The empty variant reads as the integer zero and a text that is not a number
/// reads as zero; a value with no numeric conversion at all is refused.
pub fn to_number(value: &Variant) -> Result<Variant, Unwind> {
    match value {
        Variant::Integer(_) | Variant::Real(_) => Ok(value.clone()),
        _ => match value.to_number() {
            Ok(Number::Integer(number)) => Ok(Variant::Integer(number)),
            Ok(Number::Real(number)) => Ok(Variant::Real(number)),
            Err(error) => Err(refused_conversion(error, value)),
        },
    }
}

/// The truth of a value.
///
/// The empty variant and a null object are false; an object is true when it has
/// one; a text is true when it reads as a number that is not zero; an octet is
/// true when it holds a byte; a number is true when it is not zero.
pub fn truth(value: &Variant) -> Result<bool, Unwind> {
    match value {
        Variant::Void => Ok(false),
        Variant::Integer(number) => Ok(*number != 0),
        Variant::Real(number) => Ok(*number != 0.0),
        Variant::Object(closure) => Ok(closure.object.is_some()),
        Variant::Octet(octet) => Ok(!octet.is_empty()),
        Variant::String(_) => match value.to_integer() {
            Ok(number) => Ok(number != 0),
            Err(error) => Err(refused_conversion(error, value)),
        },
    }
}

/// The truth of a value as the `!` operator answers it.
///
/// The answer is an integer, not a separate boolean kind.
pub fn logical_not(value: &Variant) -> Result<Variant, Unwind> {
    Ok(Variant::Integer(i64::from(!truth(value)?)))
}

/// Whether two values are equal in the strict comparison.
///
/// Two values of one kind compare by that kind and nothing else: an object
/// compares by both of its handles, so two closures over one object with
/// different receivers are *not* equal, and a real compares by value with
/// not-a-number equal to nothing. Two values of different kinds are never
/// equal — which is the whole difference from the plain comparison, where the
/// empty variant equals the integer zero and a text compares as text.
pub fn discern_equals(left: &Variant, right: &Variant) -> bool {
    match (left, right) {
        (Variant::Void, Variant::Void) => true,
        (Variant::Integer(left), Variant::Integer(right)) => left == right,
        (Variant::Real(left), Variant::Real(right)) => {
            !left.is_nan() && !right.is_nan() && left == right
        }
        (Variant::String(left), Variant::String(right)) => left == right,
        (Variant::Octet(left), Variant::Octet(right)) => left.as_slice() == right.as_slice(),
        (Variant::Object(left), Variant::Object(right)) => {
            same_handle(&left.object, &right.object) && same_handle(&left.objthis, &right.objthis)
        }
        _ => false,
    }
}

/// Whether two optional object handles name the same object.
///
/// An absent handle names no object, and two absent handles are the same
/// nothing.
fn same_handle(left: &Option<Rc<dyn Dispatch>>, right: &Option<Rc<dyn Dispatch>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        _ => false,
    }
}

/// The value of `>>>`: an unsigned shift of the left operand's integer.
///
/// The variant's own right shift is arithmetic, so this one is not its call. The
/// count is the low six bits, which is what the hardware does with a shift count
/// and what the variant's own shifts do.
pub fn logical_shift_right(left: &Variant, right: &Variant) -> Result<Variant, Unwind> {
    let value = left
        .to_integer()
        .map_err(|error| refused_conversion(error, left))?;
    let count = right
        .to_integer()
        .map_err(|error| refused_conversion(error, right))?;
    // the mask bounds the count to 0..=63, so the conversion cannot fail
    let count = (count & 63) as u32;
    // the value shifts as an unsigned integer and the bits are stored back into
    // a signed one
    Ok(Variant::Integer(((value as u64) >> count) as i64))
}

/// The comparison the less-than instruction asks for.
///
/// The comparison answers whether the *other* operand is greater, which is the
/// same question as whether this one is less.
pub fn less(left: &Variant, right: &Variant) -> Result<bool, Unwind> {
    ordering(left, right, Ordering::Less)
}

/// The comparison the greater-than instruction asks for.
pub fn greater(left: &Variant, right: &Variant) -> Result<bool, Unwind> {
    ordering(left, right, Ordering::Greater)
}

/// Whether the comparison of the two values is `wanted`.
///
/// An unordered pair — one operand is a not-a-number — is false for both
/// operators, and a value with no ordering at all is refused.
fn ordering(left: &Variant, right: &Variant, wanted: Ordering) -> Result<bool, Unwind> {
    match left.compare(right) {
        Ok(Some(order)) => Ok(order == wanted),
        Ok(None) => Ok(false),
        Err(error) => Err(refused_conversion(error, left)),
    }
}

/// The code of a text's first unit, as the `#` operator answers it.
///
/// A value that has no text is refused; an empty text answers zero, which is the
/// terminator an empty text holds.
pub fn character_code(value: &Variant) -> Result<Variant, Unwind> {
    let text = value
        .to_string()
        .map_err(|error| refused_conversion(error, value))?;
    let code = text.as_utf16().first().copied().unwrap_or(0);
    Ok(Variant::Integer(i64::from(code)))
}

/// The one-unit text of a code, as the `$` operator answers it.
///
/// The code is truncated to a unit, and a code of zero is the empty text because
/// a text ends at its first zero unit.
pub fn character_from(value: &Variant) -> Result<Variant, Unwind> {
    let code = value
        .to_integer()
        .map_err(|error| refused_conversion(error, value))?;
    // the code is stored into a unit, which keeps the low sixteen bits
    let units = [code as u16];
    Ok(Variant::String(TjsString::from(&units[..])))
}

/// The class name a value names, for `instanceof`.
pub fn class_name(value: &Variant) -> Result<TjsString, Unwind> {
    value
        .to_string()
        .map_err(|error| refused_conversion(error, value))
}

/// A variant failure as a raise, naming the operand the operator converts
/// first.
fn raised(error: VariantError, left: &Variant, right: &Variant, operator: Operator) -> Unwind {
    let operand = if operator.right_first() { right } else { left };
    refused_conversion(error, operand)
}

/// A variant failure as a raise, naming the value it was asked of.
pub fn refused_conversion(error: VariantError, value: &Variant) -> Unwind {
    match error {
        VariantError::Convert { to } => Unwind::raised(Raise::Convert { to }).with_value(value),
        VariantError::DivideByZero => Unwind::raised(Raise::DivideByZero),
        VariantError::IntegerOutOfRange => Unwind::raised(Raise::IntegerOutOfRange),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::runtime::exec::Fault;
    use crate::vm::runtime::object::CustomObject;
    use crate::vm::runtime::octet::Octet;
    use crate::vm::runtime::variant::Closure;

    fn text(value: &str) -> Variant {
        Variant::String(TjsString::from(value))
    }

    fn integer(value: i64) -> Variant {
        Variant::Integer(value)
    }

    fn an_object() -> Rc<dyn Dispatch> {
        Rc::new(CustomObject::new(Vec::new()))
    }

    fn object_value() -> Variant {
        let object = an_object();
        Variant::Object(Closure {
            object: Some(object.clone()),
            objthis: Some(object),
        })
    }

    /// The operator answers the expected value.
    ///
    /// A variant has no equality of its own, so the comparison is the strict
    /// one, which is equality over every kind and every handle.
    #[track_caller]
    fn is(actual: Result<Variant, Unwind>, expected: Variant) {
        match actual {
            Ok(value) => assert!(
                discern_equals(&value, &expected),
                "expected {expected:?}, got {value:?}"
            ),
            Err(error) => panic!("expected {expected:?}, got {error:?}"),
        }
    }

    /// The operator answers the expected value, and the reason is the message of
    /// a failure.
    #[track_caller]
    fn is_because(actual: Result<Variant, Unwind>, expected: Variant, why: &str) {
        match actual {
            Ok(value) => assert!(
                discern_equals(&value, &expected),
                "{why}: expected {expected:?}, got {value:?}"
            ),
            Err(error) => panic!("{why}: expected {expected:?}, got {error:?}"),
        }
    }

    /// The operator raised, and the raise is the expected one.
    #[track_caller]
    fn raises(actual: Result<Variant, Unwind>, expected: Raise) {
        match actual {
            Ok(value) => panic!("expected {expected:?}, got the value {value:?}"),
            Err(unwind) => assert_eq!(unwind.fault(), Fault::Raised(expected)),
        }
    }

    /// A value is true when it is not zero, and the empty variant is never true
    /// (`reference/krkr2/cpp/core/tjs2/tjsVariant.h:911-931`, `operator bool`).
    #[test]
    fn the_truth_of_a_value_is_the_engines() {
        assert!(!truth(&Variant::Void).expect("a truth"));
        assert!(!truth(&integer(0)).expect("a truth"));
        assert!(truth(&integer(1)).expect("a truth"));
        assert!(truth(&integer(-1)).expect("a truth"));
        assert!(!truth(&Variant::Real(0.0)).expect("a truth"));
        assert!(!truth(&Variant::Real(-0.0)).expect("a truth"));
        assert!(truth(&Variant::Real(0.5)).expect("a truth"));
        assert!(!truth(&text("")).expect("a truth"));
        assert!(!truth(&text("0")).expect("a truth"));
        assert!(truth(&text("2")).expect("a truth"));
        assert!(!truth(&text("abc")).expect("a truth"));
        assert!(!truth(&Variant::Octet(Octet::new())).expect("a truth"));
        assert!(
            !truth(&Variant::Object(Closure::null())).expect("a truth"),
            "a null object is false"
        );
        assert!(
            truth(&object_value()).expect("a truth"),
            "an object with one is true"
        );
    }

    /// An object is true when it has an object handle, whatever its receiver
    /// (`reference/krkr2/cpp/core/tjs2/tjsVariant.h:917-920`).
    #[test]
    fn an_object_without_a_receiver_is_still_true() {
        let value = Variant::Object(Closure {
            object: Some(an_object()),
            objthis: None,
        });
        assert!(truth(&value).expect("a truth"));
    }

    /// The strict comparison holds two values of one kind, and of one kind only
    /// (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:806-840`,
    /// `DiscernCompare`).
    #[test]
    fn the_strict_comparison_compares_one_kind_against_itself() {
        assert!(discern_equals(&Variant::Void, &Variant::Void));
        assert!(discern_equals(&integer(2), &integer(2)));
        assert!(!discern_equals(&integer(2), &Variant::Real(2.0)));
        assert!(
            !discern_equals(&integer(0), &Variant::Void),
            "the plain comparison equals these, the strict one does not"
        );
        assert!(!discern_equals(&text("2"), &integer(2)));
        assert!(discern_equals(&Variant::Real(2.0), &Variant::Real(2.0)));
        assert!(
            !discern_equals(&Variant::Real(f64::NAN), &Variant::Real(f64::NAN)),
            "a not-a-number is equal to nothing"
        );
        assert!(discern_equals(
            &Variant::Real(f64::INFINITY),
            &Variant::Real(f64::INFINITY)
        ));
        assert!(!discern_equals(
            &Variant::Real(f64::INFINITY),
            &Variant::Real(f64::NEG_INFINITY)
        ));
        assert!(discern_equals(&text("ab"), &text("ab")));
        assert!(!discern_equals(&text("ab"), &text("ac")));
    }

    /// The strict comparison compares an object by both of its handles
    /// (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:809-811`).
    #[test]
    fn the_strict_comparison_sees_the_receiver() {
        let first = an_object();
        let second = an_object();
        let bound = Variant::Object(Closure {
            object: Some(first.clone()),
            objthis: Some(second.clone()),
        });
        let same = Variant::Object(Closure {
            object: Some(first.clone()),
            objthis: Some(second.clone()),
        });
        let other_receiver = Variant::Object(Closure {
            object: Some(first.clone()),
            objthis: Some(first.clone()),
        });
        assert!(discern_equals(&bound, &same));
        assert!(
            !discern_equals(&bound, &other_receiver),
            "the receiver is half of an object's identity"
        );
        assert!(
            bound.equals(&other_receiver),
            "the plain comparison does not see the receiver"
        );
        assert!(
            discern_equals(
                &Variant::Object(Closure::null()),
                &Variant::Object(Closure::null())
            ),
            "two nulls carry the same two absent handles"
        );
        assert!(
            !discern_equals(&Variant::Object(Closure::null()), &object_value()),
            "a null and an object are not the same object"
        );
    }

    /// `>>>` shifts the integer unsigned, where `>>` shifts it arithmetically
    /// (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:958-963`,
    /// `rbitshiftequal`).
    #[test]
    fn the_unsigned_shift_is_not_the_arithmetic_one() {
        let minus_one = integer(-1);
        is(
            apply(Operator::Sr, &minus_one, &integer(1)),
            integer(0x7fff_ffff_ffff_ffff),
        );
        is(apply(Operator::Sar, &minus_one, &integer(1)), integer(-1));
        is_because(
            apply(Operator::Sr, &minus_one, &integer(63)),
            integer(1),
            "the last bit reaches the low end",
        );
        is_because(
            apply(Operator::Sr, &minus_one, &integer(64)),
            integer(-1),
            "the count is masked to six bits, so sixty-four is a shift of none",
        );
    }

    /// A compound assignment on two values is the variant's operator
    /// (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:26-73`,
    /// `TJSDoVariantOperation`).
    #[test]
    fn the_two_operand_operators_are_the_variants() {
        is(apply(Operator::Add, &integer(2), &integer(3)), integer(5));
        is(apply(Operator::Add, &text("a"), &integer(3)), text("a3"));
        is(apply(Operator::Sub, &integer(2), &integer(3)), integer(-1));
        is(apply(Operator::Mul, &integer(2), &integer(3)), integer(6));
        is(
            apply(Operator::Div, &integer(1), &integer(0)),
            Variant::Real(f64::INFINITY),
        );
        is(apply(Operator::IDiv, &integer(7), &integer(2)), integer(3));
        is(apply(Operator::Mod, &integer(7), &integer(2)), integer(1));
        raises(
            apply(Operator::IDiv, &integer(7), &integer(0)),
            Raise::DivideByZero,
        );
        raises(
            apply(Operator::Mod, &integer(7), &integer(0)),
            Raise::DivideByZero,
        );
        is(apply(Operator::BAnd, &integer(6), &integer(3)), integer(2));
        is(apply(Operator::BOr, &integer(4), &integer(3)), integer(7));
        is(apply(Operator::BXor, &integer(4), &integer(3)), integer(7));
        is(apply(Operator::Sal, &integer(1), &integer(2)), integer(4));
    }

    /// `||=` and `&&=` answer an integer, not the operand that decided the
    /// answer (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:912-925`).
    #[test]
    fn the_logical_assignments_answer_an_integer() {
        is_because(
            apply(Operator::LOr, &text("a"), &integer(0)),
            integer(0),
            "a text that is not a number reads as zero, so it is false",
        );
        is_because(
            apply(Operator::LOr, &text("1"), &integer(0)),
            integer(1),
            "a text that reads as a number that is not zero is true",
        );
        is(apply(Operator::LOr, &integer(0), &text("")), integer(0));
        is(apply(Operator::LAnd, &text("1"), &integer(2)), integer(1));
        is(apply(Operator::LAnd, &integer(3), &text("b")), integer(0));
    }

    /// A conversion the language refuses is a raise carrying the value it was
    /// asked of (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:146-163`).
    #[test]
    fn a_refused_conversion_carries_the_value() {
        match apply(Operator::BAnd, &object_value(), &integer(1)) {
            Err(unwind) => {
                assert!(
                    matches!(unwind.fault(), Fault::Raised(Raise::Convert { .. })),
                    "the failure is a conversion raise"
                );
                assert!(
                    matches!(unwind.value(), Some(Variant::Object(_))),
                    "the value travels"
                );
            }
            Ok(value) => panic!("expected a conversion raise, got the value {value:?}"),
        }
    }

    /// The remainder reads its divisor first, so a failure names the divisor
    /// (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:1008-1037`).
    #[test]
    fn the_remainder_names_the_divisor() {
        match apply(Operator::Mod, &integer(1), &object_value()) {
            Err(unwind) => {
                assert!(
                    matches!(unwind.value(), Some(Variant::Object(_))),
                    "the divisor is named"
                );
            }
            Ok(value) => panic!("expected a conversion raise, got the value {value:?}"),
        }
        match apply(Operator::BAnd, &object_value(), &integer(1)) {
            Err(unwind) => {
                assert!(
                    matches!(unwind.value(), Some(Variant::Object(_))),
                    "the dividend is named"
                );
            }
            Ok(value) => panic!("expected a conversion raise, got the value {value:?}"),
        }
    }

    /// `#` answers the first unit's code, and a value with no text is refused
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2663-2673`).
    #[test]
    fn the_character_code_of_a_text() {
        is(character_code(&text("A")), integer(65));
        is(character_code(&text("AB")), integer(65));
        is(character_code(&integer(7)), integer(55));
        is(character_code(&text("")), integer(0));
    }

    /// `$` answers a one-unit text and truncates the code to a unit
    /// (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2676-2681`).
    #[test]
    fn the_character_of_a_code() {
        is(character_from(&integer(65)), text("A"));
        is(character_from(&integer(65 + 65536)), text("A"));
        is(character_from(&integer(0)), text(""));
        is(character_from(&text("3")), text("\u{3}"));
    }

    /// `+` is the number conversion, and a value that already is a number keeps
    /// its kind (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:1073-1088`,
    /// `tonumber`).
    #[test]
    fn the_number_conversion_keeps_an_operands_kind() {
        is(to_number(&integer(2)), integer(2));
        is(to_number(&Variant::Real(2.5)), Variant::Real(2.5));
        is(to_number(&text("2")), integer(2));
        is(to_number(&text("2.5")), Variant::Real(2.5));
        is(to_number(&text("abc")), integer(0));
        is(to_number(&Variant::Void), integer(0));
    }

    /// `!` answers an integer (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:1057-1062`).
    #[test]
    fn the_logical_not_answers_an_integer() {
        is(logical_not(&integer(0)), integer(1));
        is(logical_not(&integer(5)), integer(0));
        is(logical_not(&Variant::Void), integer(1));
    }
}
