//! The constant folding.
//!
//! The generator folds the constant expressions: `1 + 2` becomes one
//! constant. The folding happens where the node is made — so the folded
//! value is what the rest of the generator sees.
//!
//! The folding follows the variant's own semantics: the operations are
//! the variant's operators, and a conversion that has no result simply
//! does not fold.

use std::rc::Rc;

use crate::vm::codegen::node::Node;
use crate::vm::codegen::token;
use crate::vm::codegen::value::Value;

/// The folded value of a unary operation, when it folds.
pub fn fold_unary(opcode: i32, operand: &Value) -> Option<Value> {
    Some(match opcode {
        token::T_EXCRAMATION => Value::Integer(!operand.truthy() as i64),
        token::T_TILDE => Value::Integer(!operand.to_integer()?),
        token::T_SHARP => {
            // `CharacterCodeOf`: the code of the value's first character
            let text = operand.to_text()?;
            let unit = text.encode_utf16().next().unwrap_or(0);
            Value::Integer(unit as i64)
        }
        token::T_DOLLAR => {
            // `CharacterCodeFrom`: the one-character string of the value's
            // code
            let code = operand.to_integer()?;
            let unit = char::from_u32((code as u32) & 0xffff)?;
            Value::String(unit.to_string())
        }
        token::T_UPLUS => {
            // `tonumber`
            operand.to_number()?
        }
        token::T_UMINUS => {
            // `changesign`: an integer keeps its kind, and every other
            // number becomes the number it reads as
            match operand {
                Value::Integer(number) => Value::Integer(number.wrapping_neg()),
                _ => match operand.to_number()? {
                    Value::Integer(number) => Value::Integer(number.wrapping_neg()),
                    Value::Real(number) => Value::Real(-number),
                    _ => return None,
                },
            }
        }
        token::T_INT => Value::Integer(operand.to_integer()?),
        token::T_REAL => Value::Real(operand.to_real()?),
        token::T_STRING => Value::String(operand.to_text()?),
        token::T_OCTET => match operand {
            // `ToOctet`: a byte string stays one, and a value of no value
            // becomes an empty one; every other value raises
            Value::Octet(bytes) => Value::Octet(bytes.clone()),
            Value::Void => Value::Octet(Vec::new()),
            _ => return None,
        },
        _ => return None,
    })
}

/// The folded value of a binary operation, when it folds.
pub fn fold_binary(opcode: i32, left: &Value, right: &Value) -> Option<Value> {
    Some(match opcode {
        token::T_COMMA => right.clone(),
        token::T_LOGICALOR => Value::Integer((left.truthy() || right.truthy()) as i64),
        token::T_LOGICALAND => Value::Integer((left.truthy() && right.truthy()) as i64),
        token::T_VERTLINE => Value::Integer(left.to_integer()? | right.to_integer()?),
        token::T_CHEVRON => Value::Integer(left.to_integer()? ^ right.to_integer()?),
        token::T_AMPERSAND => Value::Integer(left.to_integer()? & right.to_integer()?),
        token::T_NOTEQUAL => Value::Integer(!Value::normal_compare(left, right) as i64),
        token::T_EQUALEQUAL => Value::Integer(Value::normal_compare(left, right) as i64),
        token::T_DISCNOTEQUAL => Value::Integer(!Value::discern_compare(left, right) as i64),
        token::T_DISCEQUAL => Value::Integer(Value::discern_compare(left, right) as i64),
        token::T_LT => Value::Integer((compare_ordered(left, right)? < 0) as i64),
        token::T_GT => Value::Integer((compare_ordered(left, right)? > 0) as i64),
        token::T_LTOREQUAL => Value::Integer((compare_ordered(left, right)? <= 0) as i64),
        token::T_GTOREQUAL => Value::Integer((compare_ordered(left, right)? >= 0) as i64),
        token::T_RARITHSHIFT => {
            // `operator>>`
            let count = shift_count(right)?;
            Value::Integer(left.to_integer()? >> count)
        }
        token::T_LARITHSHIFT => {
            // `operator<<`
            let count = shift_count(right)?;
            Value::Integer(left.to_integer()? << count)
        }
        token::T_RBITSHIFT => {
            // `rbitshift`: the shift is an unsigned one
            let count = shift_count(right)?;
            Value::Integer(((left.to_integer()? as u64) >> count) as i64)
        }
        token::T_PLUS => add(left, right)?,
        token::T_MINUS => subtract(left, right)?,
        token::T_PERCENT => {
            // `operator%`: a zero divisor raises, and the operation does
            // not fold
            let divisor = right.to_integer()?;
            if divisor == 0 {
                return None;
            }
            Value::Integer(left.to_integer()? % divisor)
        }
        token::T_SLASH => {
            // `operator/`: the quotient is always a real
            Value::Real(left.to_real()? / right.to_real()?)
        }
        token::T_BACKSLASH => {
            // `idiv`: a zero divisor raises, and the operation does not
            // fold
            let divisor = right.to_integer()?;
            if divisor == 0 {
                return None;
            }
            Value::Integer(left.to_integer()? / divisor)
        }
        token::T_ASTERISK => multiply(left, right)?,
        _ => return None,
    })
}

/// The sum of two values.
///
/// `operator+`: a string on either side makes the sum a string, two byte
/// strings make a byte string, two integers make an integer, and a value
/// of no value is the other side's value.
fn add(left: &Value, right: &Value) -> Option<Value> {
    if matches!(left, Value::String(_)) || matches!(right, Value::String(_)) {
        // the sum is a string
        let mut text = left.to_text()?;
        text.push_str(&right.to_text()?);
        return Some(Value::String(text));
    }
    if let (Value::Octet(left), Value::Octet(right)) = (left, right) {
        // the sum is a byte string
        let mut bytes = left.clone();
        bytes.extend_from_slice(right);
        return Some(Value::Octet(bytes));
    }
    if let (Value::Integer(left), Value::Integer(right)) = (left, right) {
        // the sum is an integer
        return Some(Value::Integer(left.wrapping_add(*right)));
    }
    if let Value::Void = left
        && matches!(right, Value::Integer(_) | Value::Real(_))
    {
        return Some(right.clone());
    }
    if let Value::Void = right
        && matches!(left, Value::Integer(_) | Value::Real(_))
    {
        return Some(left.clone());
    }
    // every other sum is a real
    Some(Value::Real(left.to_real()? + right.to_real()?))
}

/// The difference of two values.
///
/// `operator-=` and `InternalSub`: both sides are read as numbers, and
/// two integers make an integer.
fn subtract(left: &Value, right: &Value) -> Option<Value> {
    let left = left.to_number()?;
    let right = right.to_number()?;
    if let (Value::Integer(left), Value::Integer(right)) = (&left, &right) {
        return Some(Value::Integer(left.wrapping_sub(*right)));
    }
    Some(Value::Real(left.to_real()? - right.to_real()?))
}

/// The product of two values.
///
/// `operator*=` and `InternalMul`: both sides are read as numbers, and
/// two integers make an integer.
fn multiply(left: &Value, right: &Value) -> Option<Value> {
    let left = left.to_number()?;
    let right = right.to_number()?;
    if let (Value::Integer(left), Value::Integer(right)) = (&left, &right) {
        return Some(Value::Integer(left.wrapping_mul(*right)));
    }
    Some(Value::Real(left.to_real()? * right.to_real()?))
}

/// The count of a shift.
///
/// The shift is by the count the value reads as, and the count is a
/// machine word: a count that does not fit is taken modulo the width of
/// the shift, the way the machine does it.
fn shift_count(value: &Value) -> Option<u32> {
    let count = value.to_integer()?;
    Some((count as u64 & 63) as u32)
}

/// The ordered comparison of two values.
///
/// The comparison is numeric: the two values are compared as reals.
fn compare_ordered(left: &Value, right: &Value) -> Option<i32> {
    if let (Value::String(left), Value::String(right)) = (left, right) {
        return Some(match left.cmp(right) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        });
    }
    if let (Value::Integer(left), Value::Integer(right)) = (left, right) {
        return Some(match left.cmp(right) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        });
    }
    let left = left.to_real()?;
    let right = right.to_real()?;
    Some(if left < right {
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
