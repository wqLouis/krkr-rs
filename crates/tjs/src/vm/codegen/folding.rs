//! The constant folding.
//!
//! The generator folds the constant expressions: `1 + 2` becomes one
//! constant. The folding happens where the node is made — so the folded
//! value is what the rest of the generator sees.
//!
//! The folding follows the variant's own semantics: the operations are
//! the variant's operators, and a conversion the engine refuses makes
//! the fold refuse as well. A refused expression does not fold: it keeps
//! the shape it was written with, and the generator refuses it when it
//! meets the node, with the engine's own message.

use std::rc::Rc;

use crate::vm::codegen::node::Node;
use crate::vm::codegen::token;
use crate::vm::codegen::value::{Raise, Target, Value};

/// The result of a fold.
///
/// The fold gives the value the expression folds to, or the refusal the
/// engine raises for it, or says that the expression does not fold at
/// all.
#[derive(Clone, Debug, PartialEq)]
enum Fold {
    /// The expression does not fold: the code is generated as it is
    /// written.
    NoFold,
    /// The expression folds to this value.
    Value(Value),
    /// The engine refuses the expression: the script is refused while it
    /// is compiled.
    Refused(Raise),
}

/// The folded value of a unary operation, when it folds.
///
/// The engine refuses some of the operations, and a refused operation
/// does not fold: the node keeps the shape it was written with, and
/// [`refusal_of`] names the refusal when the generator meets the node.
pub fn fold_unary(opcode: i32, operand: &Value) -> Option<Value> {
    match fold_unary_operation(opcode, operand) {
        Fold::Value(value) => Some(value),
        Fold::NoFold | Fold::Refused(_) => None,
    }
}

/// The folded value of a binary operation, when it folds.
///
/// The engine refuses some of the operations, and a refused operation
/// does not fold: the node keeps the shape it was written with, and
/// [`refusal_of`] names the refusal when the generator meets the node.
pub fn fold_binary(opcode: i32, left: &Value, right: &Value) -> Option<Value> {
    match fold_binary_operation(opcode, left, right) {
        Fold::Value(value) => Some(value),
        Fold::NoFold | Fold::Refused(_) => None,
    }
}

/// The refusal the engine raises for the node, when it raises one.
///
/// The node is folded the way the parser folds it — the same fold, over
/// the same operands — and the fold refuses when the engine refuses.
pub fn refusal_of(node: &Rc<Node>) -> Option<Raise> {
    match node.len() {
        1 => {
            let operand = constant(node.child(0))?;
            match fold_unary_operation(node.opcode, &operand) {
                Fold::Refused(raise) => Some(raise),
                Fold::NoFold | Fold::Value(_) => None,
            }
        }
        2 => {
            let left = constant(node.child(0))?;
            let right = constant(node.child(1))?;
            match fold_binary_operation(node.opcode, &left, &right) {
                Fold::Refused(raise) => Some(raise),
                Fold::NoFold | Fold::Value(_) => None,
            }
        }
        _ => None,
    }
}

/// The folded value of a unary operation.
fn fold_unary_operation(opcode: i32, operand: &Value) -> Fold {
    match opcode {
        token::T_EXCRAMATION => Fold::Value(Value::Integer(!operand.truthy() as i64)),
        token::T_TILDE => match operand.to_integer() {
            Ok(number) => Fold::Value(Value::Integer(!number)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_SHARP => {
            // `CharacterCodeOf`: the code of the value's first character
            match operand.to_text() {
                Ok(text) => {
                    let unit = text.encode_utf16().next().unwrap_or(0);
                    Fold::Value(Value::Integer(unit as i64))
                }
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_DOLLAR => {
            // `CharacterCodeFrom`: the one-character string of the value's
            // code
            match operand.to_integer() {
                Ok(code) => match char::from_u32((code as u32) & 0xffff) {
                    Some(unit) => Fold::Value(Value::String(unit.to_string())),
                    None => Fold::NoFold,
                },
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_UPLUS => match operand.to_number() {
            Ok(value) => Fold::Value(value),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_UMINUS => {
            // `changesign`: an integer keeps its kind, and every other
            // number becomes the number it reads as
            match operand {
                Value::Integer(number) => Fold::Value(Value::Integer(number.wrapping_neg())),
                _ => match operand.to_number() {
                    Ok(Value::Integer(number)) => {
                        Fold::Value(Value::Integer(number.wrapping_neg()))
                    }
                    Ok(Value::Real(number)) => Fold::Value(Value::Real(-number)),
                    Ok(_) => Fold::NoFold,
                    Err(raise) => Fold::Refused(raise),
                },
            }
        }
        token::T_INT => match operand.to_integer() {
            Ok(number) => Fold::Value(Value::Integer(number)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_REAL => match operand.to_real() {
            Ok(number) => Fold::Value(Value::Real(number)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_STRING => match operand.to_text() {
            Ok(text) => Fold::Value(Value::String(text)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_OCTET => match operand {
            // `ToOctet`: a byte string stays one, and a value of no value
            // becomes an empty one; every other value is refused
            Value::Octet(bytes) => Fold::Value(Value::Octet(bytes.clone())),
            Value::Void => Fold::Value(Value::Octet(Vec::new())),
            _ => Fold::Refused(operand.refuse(Target::Octet)),
        },
        _ => Fold::NoFold,
    }
}

/// The folded value of a binary operation.
fn fold_binary_operation(opcode: i32, left: &Value, right: &Value) -> Fold {
    match opcode {
        token::T_COMMA => Fold::Value(right.clone()),
        token::T_LOGICALOR => Fold::Value(Value::Integer((left.truthy() || right.truthy()) as i64)),
        token::T_LOGICALAND => {
            Fold::Value(Value::Integer((left.truthy() && right.truthy()) as i64))
        }
        token::T_VERTLINE => integer_operation(left, right, |a, b| a | b),
        token::T_CHEVRON => integer_operation(left, right, |a, b| a ^ b),
        token::T_AMPERSAND => integer_operation(left, right, |a, b| a & b),
        token::T_NOTEQUAL => {
            Fold::Value(Value::Integer(!Value::normal_compare(left, right) as i64))
        }
        token::T_EQUALEQUAL => {
            Fold::Value(Value::Integer(Value::normal_compare(left, right) as i64))
        }
        token::T_DISCNOTEQUAL => {
            Fold::Value(Value::Integer(!Value::discern_compare(left, right) as i64))
        }
        token::T_DISCEQUAL => {
            Fold::Value(Value::Integer(Value::discern_compare(left, right) as i64))
        }
        token::T_LT => match compare_ordered(left, right) {
            Ok(order) => Fold::Value(Value::Integer((order < 0) as i64)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_GT => match compare_ordered(left, right) {
            Ok(order) => Fold::Value(Value::Integer((order > 0) as i64)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_LTOREQUAL => match compare_ordered(left, right) {
            Ok(order) => Fold::Value(Value::Integer((order <= 0) as i64)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_GTOREQUAL => match compare_ordered(left, right) {
            Ok(order) => Fold::Value(Value::Integer((order >= 0) as i64)),
            Err(raise) => Fold::Refused(raise),
        },
        token::T_RARITHSHIFT => {
            // `operator>>`
            match shift_count(right) {
                Ok(count) => match left.to_integer() {
                    Ok(number) => Fold::Value(Value::Integer(number >> count)),
                    Err(raise) => Fold::Refused(raise),
                },
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_LARITHSHIFT => {
            // `operator<<`
            match shift_count(right) {
                Ok(count) => match left.to_integer() {
                    Ok(number) => Fold::Value(Value::Integer(number << count)),
                    Err(raise) => Fold::Refused(raise),
                },
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_RBITSHIFT => {
            // `rbitshift`: the shift is an unsigned one
            match shift_count(right) {
                Ok(count) => match left.to_integer() {
                    Ok(number) => Fold::Value(Value::Integer(((number as u64) >> count) as i64)),
                    Err(raise) => Fold::Refused(raise),
                },
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_PLUS => add(left, right),
        token::T_MINUS => subtract(left, right),
        token::T_PERCENT => {
            // `operator%`: the divisor is read first, a zero divisor is
            // refused, and the dividend is read after it
            match right.to_integer() {
                Ok(divisor) => {
                    if divisor == 0 {
                        Fold::Refused(Raise::DivideByZero)
                    } else {
                        match left.to_integer() {
                            Ok(dividend) => {
                                Fold::Value(Value::Integer(dividend.wrapping_rem(divisor)))
                            }
                            Err(raise) => Fold::Refused(raise),
                        }
                    }
                }
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_SLASH => {
            // `operator/`: the quotient is always a real
            match right.to_real() {
                Ok(divisor) => match left.to_real() {
                    Ok(dividend) => Fold::Value(Value::Real(dividend / divisor)),
                    Err(raise) => Fold::Refused(raise),
                },
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_BACKSLASH => {
            // `idiv`: the divisor is read first, a zero divisor is
            // refused, and the dividend is read after it
            match right.to_integer() {
                Ok(divisor) => {
                    if divisor == 0 {
                        Fold::Refused(Raise::DivideByZero)
                    } else {
                        match left.to_integer() {
                            Ok(dividend) => {
                                Fold::Value(Value::Integer(dividend.wrapping_div(divisor)))
                            }
                            Err(raise) => Fold::Refused(raise),
                        }
                    }
                }
                Err(raise) => Fold::Refused(raise),
            }
        }
        token::T_ASTERISK => multiply(left, right),
        _ => Fold::NoFold,
    }
}

/// The folded value of an operation on two integers.
///
/// The left operand is read first, and the right one after it.
fn integer_operation(left: &Value, right: &Value, operation: impl Fn(i64, i64) -> i64) -> Fold {
    match left.to_integer() {
        Ok(left) => match right.to_integer() {
            Ok(right) => Fold::Value(Value::Integer(operation(left, right))),
            Err(raise) => Fold::Refused(raise),
        },
        Err(raise) => Fold::Refused(raise),
    }
}

/// The sum of two values.
///
/// `operator+`: a string on either side makes the sum a string, two byte
/// strings make a byte string, two integers make an integer, and a value
/// of no value is the other side's value.
fn add(left: &Value, right: &Value) -> Fold {
    if matches!(left, Value::String(_)) || matches!(right, Value::String(_)) {
        // the sum is a string
        return match (left.to_text(), right.to_text()) {
            (Ok(mut text), Ok(other)) => {
                text.push_str(&other);
                Fold::Value(Value::String(text))
            }
            (Err(raise), _) | (_, Err(raise)) => Fold::Refused(raise),
        };
    }
    if let (Value::Octet(left), Value::Octet(right)) = (left, right) {
        // the sum is a byte string
        let mut bytes = left.clone();
        bytes.extend_from_slice(right);
        return Fold::Value(Value::Octet(bytes));
    }
    if let (Value::Integer(left), Value::Integer(right)) = (left, right) {
        // the sum is an integer
        return Fold::Value(Value::Integer(left.wrapping_add(*right)));
    }
    if let Value::Void = left
        && matches!(right, Value::Integer(_) | Value::Real(_))
    {
        return Fold::Value(right.clone());
    }
    if let Value::Void = right
        && matches!(left, Value::Integer(_) | Value::Real(_))
    {
        return Fold::Value(left.clone());
    }
    // every other sum is a real
    match (left.to_real(), right.to_real()) {
        (Ok(left), Ok(right)) => Fold::Value(Value::Real(left + right)),
        (Err(raise), _) | (_, Err(raise)) => Fold::Refused(raise),
    }
}

/// The difference of two values.
///
/// `operator-=` and `InternalSub`: both sides are read as numbers, and
/// two integers make an integer.
fn subtract(left: &Value, right: &Value) -> Fold {
    let left = match left.to_number() {
        Ok(value) => value,
        Err(raise) => return Fold::Refused(raise),
    };
    let right = match right.to_number() {
        Ok(value) => value,
        Err(raise) => return Fold::Refused(raise),
    };
    if let (Value::Integer(left), Value::Integer(right)) = (&left, &right) {
        return Fold::Value(Value::Integer(left.wrapping_sub(*right)));
    }
    match (left.to_real(), right.to_real()) {
        (Ok(left), Ok(right)) => Fold::Value(Value::Real(left - right)),
        (Err(raise), _) | (_, Err(raise)) => Fold::Refused(raise),
    }
}

/// The product of two values.
///
/// `operator*=` and `InternalMul`: both sides are read as numbers, and
/// two integers make an integer.
fn multiply(left: &Value, right: &Value) -> Fold {
    let left = match left.to_number() {
        Ok(value) => value,
        Err(raise) => return Fold::Refused(raise),
    };
    let right = match right.to_number() {
        Ok(value) => value,
        Err(raise) => return Fold::Refused(raise),
    };
    if let (Value::Integer(left), Value::Integer(right)) = (&left, &right) {
        return Fold::Value(Value::Integer(left.wrapping_mul(*right)));
    }
    match (left.to_real(), right.to_real()) {
        (Ok(left), Ok(right)) => Fold::Value(Value::Real(left * right)),
        (Err(raise), _) | (_, Err(raise)) => Fold::Refused(raise),
    }
}

/// The count of a shift.
///
/// The shift is by the count the value reads as, and the count is a
/// machine word: a count that does not fit is taken modulo the width of
/// the shift, the way the machine does it.
fn shift_count(value: &Value) -> Result<u32, Raise> {
    let count = value.to_integer()?;
    Ok((count as u64 & 63) as u32)
}

/// The ordered comparison of two values.
///
/// The comparison is numeric: the two values are compared as reals.
fn compare_ordered(left: &Value, right: &Value) -> Result<i32, Raise> {
    if let (Value::String(left), Value::String(right)) = (left, right) {
        return Ok(match left.cmp(right) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        });
    }
    if let (Value::Integer(left), Value::Integer(right)) = (left, right) {
        return Ok(match left.cmp(right) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        });
    }
    let left = left.to_real()?;
    let right = right.to_real()?;
    Ok(if left < right {
        -1
    } else if left > right {
        1
    } else {
        0
    })
}

/// Whether the node is a constant.
fn is_const(node: Option<&Rc<Node>>) -> bool {
    node.is_some_and(|node| node.opcode == token::T_CONSTVAL)
}

/// The value of a constant node.
fn const_value(node: Option<&Rc<Node>>) -> Option<Value> {
    node.and_then(|node| node.value().cloned())
}

/// The value of a node, when the node is a constant.
fn constant(node: Option<&Rc<Node>>) -> Option<Value> {
    let node = node?;
    if !node.is_const() {
        return None;
    }
    node.value().cloned()
}

/// Makes a node with an opcode, folding it when the operand folds.
pub fn make_np1(opcode: i32, position: i32, child: Option<&Rc<Node>>) -> Rc<Node> {
    if is_const(child)
        && let Some(value) = const_value(child)
        && let Some(folded) = fold_unary(opcode, &value)
    {
        return Node::with_value(token::T_CONSTVAL, position, folded);
    }
    let mut node = Node::new(opcode, position);
    if let Some(child) = child {
        crate::vm::codegen::node::add_child(&mut node, child.clone());
    }
    node
}

/// Makes a node with an opcode and two children, folding it when the
/// operands fold.
pub fn make_np2(
    opcode: i32,
    position: i32,
    child1: Option<&Rc<Node>>,
    child2: Option<&Rc<Node>>,
) -> Rc<Node> {
    if is_const(child1)
        && is_const(child2)
        && let (Some(left), Some(right)) = (const_value(child1), const_value(child2))
        && let Some(folded) = fold_binary(opcode, &left, &right)
    {
        return Node::with_value(token::T_CONSTVAL, position, folded);
    }
    let mut node = Node::new(opcode, position);
    if let Some(child) = child1 {
        crate::vm::codegen::node::add_child(&mut node, child.clone());
    }
    if let Some(child) = child2 {
        crate::vm::codegen::node::add_child(&mut node, child.clone());
    }
    node
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::vm::codegen::node::Node;
    use crate::vm::codegen::token;
    use crate::vm::codegen::value::{ObjectId, Value};

    use super::{fold_binary, make_np1, make_np2, refusal_of};

    /// The node of a constant operation.
    fn node(opcode: i32, left: Value, right: Value) -> Rc<Node> {
        make_np2(
            opcode,
            0,
            Some(&Node::with_value(token::T_CONSTVAL, 0, left)),
            Some(&Node::with_value(token::T_CONSTVAL, 0, right)),
        )
    }

    /// The remainder of two constants refuses a zero divisor.
    ///
    /// The engine folds the constant expressions with the variant's own
    /// operators, and the operator of the remainder refuses a zero
    /// divisor
    /// (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3888,
    /// reference/krkr2/cpp/core/tjs2/tjsVariant.h:1096,
    /// reference/krkr2/cpp/core/tjs2/tjsError.h:308).
    #[test]
    fn a_remainder_by_zero_is_refused() {
        let refusal = refusal_of(&node(
            token::T_PERCENT,
            Value::Integer(0),
            Value::Integer(0),
        ))
        .expect("the zero divisor is refused");
        assert_eq!(refusal.to_string(), "Division by zero");
    }

    /// The quotient of two constants refuses a conversion the engine has
    /// no result for.
    ///
    /// The operator of the division reads both sides as reals, and the
    /// read of an object is refused
    /// (reference/krkr2/cpp/core/tjs2/tjsVariant.h:1105,
    /// reference/krkr2/cpp/core/tjs2/tjsVariant.h:937,
    /// reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:146).
    #[test]
    fn a_quotient_of_an_object_is_refused() {
        let refusal = refusal_of(&node(
            token::T_SLASH,
            Value::Object(ObjectId::NULL),
            Value::Integer(0),
        ))
        .expect("the conversion is refused");
        assert_eq!(
            refusal.to_string(),
            "Cannot convert the variable type ((object)(object 0x0:0x0) to real)"
        );
    }

    /// The bitwise complement of the null refuses the integer conversion.
    ///
    /// The operator of the complement reads its operand as an integer,
    /// and the read of an object is refused
    /// (reference/krkr2/cpp/core/tjs2/tjsVariant.h:1148,
    /// reference/krkr2/cpp/core/tjs2/tjsVariant.h:862).
    #[test]
    fn a_complement_of_an_object_is_refused() {
        let operand = Node::with_value(token::T_CONSTVAL, 0, Value::Object(ObjectId::NULL));
        let node = make_np1(token::T_TILDE, 0, Some(&operand));
        let refusal = refusal_of(&node).expect("the conversion is refused");
        assert_eq!(
            refusal.to_string(),
            "Cannot convert the variable type ((object)(object 0x0:0x0) to int)"
        );
    }

    /// An operation the engine does not refuse still folds.
    ///
    /// The folding of the constant expressions
    /// (reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3888).
    #[test]
    fn a_constant_operation_folds() {
        assert_eq!(
            fold_binary(token::T_PLUS, &Value::Integer(1), &Value::Integer(2)),
            Some(Value::Integer(3))
        );
        assert_eq!(
            fold_binary(token::T_PERCENT, &Value::Integer(7), &Value::Integer(3)),
            Some(Value::Integer(1))
        );
    }
}
