//! The conditional-compilation statements.
//!
//! A script may hide parts of itself behind a conditional-compilation
//! statement: `@if` keeps the text that follows when an expression is
//! true, `@set` gives a name a value, and `@endif` closes the block. The
//! expressions are whole numbers and names; a name that was never set is
//! zero.

use std::collections::HashMap;

use super::{Comment, Cursor, LexError, Literal, Statement, is_digit, is_letter, is_space};

/// The values the conditional-compilation statements read and write.
///
/// The values outlive a script: a value one script sets is visible in the
/// next one.
#[derive(Debug, Default)]
pub struct PpValues {
    entries: HashMap<String, i32>,
}

impl PpValues {
    /// An empty set of values.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of a name, or zero when the name was never set.
    pub fn get(&self, name: &str) -> i32 {
        self.entries.get(name).copied().unwrap_or(0)
    }

    /// The value of a name.
    pub fn set(&mut self, name: &str, value: i32) {
        self.entries.insert(name.to_string(), value);
    }
}

impl super::Lexer {
    /// The conditional-compilation statement at the current position.
    ///
    /// The cursor is at the `@`.
    pub(crate) fn scan_conditional_statement(&mut self) -> Result<Statement, LexError> {
        let start = self.cursor.position();
        self.cursor.advance();
        if self.starts_with("set") {
            return self.scan_conditional_set();
        }
        if self.starts_with("if") {
            return self.scan_conditional_if();
        }
        if self.starts_with("endif") {
            return self.scan_conditional_endif();
        }
        self.cursor.move_to(start);
        Ok(Statement::Absent)
    }

    /// Whether the text at the cursor starts with the given word.
    fn starts_with(&self, word: &str) -> bool {
        let start = self.cursor.position();
        word.bytes().enumerate().all(|(index, expected)| {
            self.cursor.unit_at(start + index) == Some(u16::from(expected))
        })
    }

    /// The `@set` statement at the cursor.
    fn scan_conditional_set(&mut self) -> Result<Statement, LexError> {
        self.cursor.move_to(self.cursor.position() + 3);
        let text = self.conditional_expression()?;
        evaluate(&text, &mut self.conditional_values).map_err(|_| self.conditional_error())?;
        self.cursor.skip_space();
        if self.cursor.current().is_none() {
            return Ok(Statement::End);
        }
        Ok(Statement::Continue)
    }

    /// The `@if` statement at the cursor.
    fn scan_conditional_if(&mut self) -> Result<Statement, LexError> {
        self.cursor.move_to(self.cursor.position() + 2);
        let text = self.conditional_expression()?;
        let value =
            evaluate(&text, &mut self.conditional_values).map_err(|_| self.conditional_error())?;
        if value == 0 {
            return self.skip_until_endif();
        }
        self.conditional_level += 1;
        self.cursor.skip_space();
        if self.cursor.current().is_none() {
            return Ok(Statement::End);
        }
        Ok(Statement::Continue)
    }

    /// The `@endif` statement at the cursor.
    fn scan_conditional_endif(&mut self) -> Result<Statement, LexError> {
        self.cursor.move_to(self.cursor.position() + 5);
        self.conditional_level -= 1;
        if self.conditional_level < 0 {
            return Err(self.conditional_error());
        }
        self.cursor.skip_space();
        if self.cursor.current().is_none() {
            return Ok(Statement::End);
        }
        Ok(Statement::Continue)
    }

    /// The expression of a conditional-compilation statement.
    ///
    /// The cursor is at the `@`; it is left after the closing parenthesis.
    fn conditional_expression(&mut self) -> Result<Vec<u16>, LexError> {
        self.cursor.skip_space();
        if self.cursor.current() != Some(u16::from(b'(')) {
            return Err(self.conditional_error());
        }
        self.cursor.advance();
        let start = self.cursor.position();
        let mut level = 0;
        loop {
            let Some(unit) = self.cursor.current() else {
                return Err(self.conditional_error());
            };
            if unit == u16::from(b'(') {
                level += 1;
            } else if unit == u16::from(b')') {
                if level == 0 {
                    break;
                }
                level -= 1;
            }
            self.cursor.advance();
        }
        let end = self.cursor.position();
        self.cursor.advance();
        Ok(self.collect_expression(start, end))
    }

    /// The code units of an expression, without the newline markers.
    fn collect_expression(&self, start: usize, end: usize) -> Vec<u16> {
        self.cursor
            .units()
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|&unit| unit != super::SKIP_CODE)
            .collect()
    }

    /// The text up to the `@endif` that closes the block.
    fn skip_until_endif(&mut self) -> Result<Statement, LexError> {
        let outer = self.conditional_level;
        self.conditional_level += 1;
        loop {
            let Some(unit) = self.cursor.current() else {
                return Err(self.conditional_error());
            };
            if unit == u16::from(b'/') {
                match self.cursor.skip_comment() {
                    Comment::Skipped => continue,
                    Comment::Ended => return Err(self.conditional_error()),
                    Comment::Unterminated => return Err(self.conditional_error()),
                    Comment::Absent => {
                        if !self.cursor.advance() {
                            return Err(self.conditional_error());
                        }
                    }
                }
            } else if unit == u16::from(b'@') {
                self.cursor.advance();
                let mut skip_parenthesised = false;
                if self.starts_with("if") {
                    self.conditional_level += 1;
                    self.cursor.move_to(self.cursor.position() + 2);
                    skip_parenthesised = true;
                } else if self.starts_with("set") {
                    self.cursor.move_to(self.cursor.position() + 3);
                    skip_parenthesised = true;
                } else if self.starts_with("endif") {
                    self.cursor.move_to(self.cursor.position() + 5);
                    self.conditional_level -= 1;
                    if self.conditional_level == outer {
                        self.cursor.skip_space();
                        if self.cursor.current().is_none() {
                            return Ok(Statement::End);
                        }
                        return Ok(Statement::Continue);
                    }
                }
                if skip_parenthesised {
                    self.cursor.skip_space();
                    if self.cursor.current() != Some(u16::from(b'(')) {
                        return Err(self.conditional_error());
                    }
                    self.cursor.advance();
                    let mut level = 0;
                    loop {
                        let Some(unit) = self.cursor.current() else {
                            return Err(self.conditional_error());
                        };
                        if unit == u16::from(b'(') {
                            level += 1;
                        } else if unit == u16::from(b')') {
                            if level == 0 {
                                break;
                            }
                            level -= 1;
                        }
                        self.cursor.advance();
                    }
                    self.cursor.advance();
                    if self.cursor.current().is_none() {
                        return Err(self.conditional_error());
                    }
                }
            } else {
                if !self.cursor.advance() {
                    return Err(self.conditional_error());
                }
            }
        }
    }

    /// The error for a malformed conditional-compilation statement.
    fn conditional_error(&self) -> LexError {
        LexError::ConditionalCompilation {
            position: self.cursor.position(),
        }
    }
}

/// The failure of a conditional-compilation expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpressionError {
    /// The expression is not well formed.
    Syntax,
    /// The expression divides by zero.
    DivisionByZero,
}

/// A piece of a conditional-compilation expression.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Piece {
    /// A number.
    Number(i32),
    /// A name.
    Symbol(String),
    /// An operator or a parenthesis.
    Operator(&'static str),
    /// The end of the expression.
    End,
}

/// A value of a conditional-compilation expression, and the name it was
/// written as.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Operand {
    value: i32,
    symbol: Option<String>,
}

/// The value of a conditional-compilation expression.
fn evaluate(text: &[u16], values: &mut PpValues) -> Result<i32, ExpressionError> {
    let mut expression = Expression {
        units: text,
        position: 0,
        values,
    };
    let operand = expression.parse_binary(1)?;
    if !matches!(expression.peek()?, Piece::End) {
        return Err(ExpressionError::Syntax);
    }
    Ok(operand.value)
}

/// A conditional-compilation expression.
struct Expression<'a> {
    units: &'a [u16],
    position: usize,
    values: &'a mut PpValues,
}

impl Expression<'_> {
    /// The next piece of the expression.
    fn next(&mut self) -> Result<Piece, ExpressionError> {
        while let Some(unit) = self.units.get(self.position) {
            if is_space(*unit) {
                self.position += 1;
            } else {
                break;
            }
        }
        let Some(&unit) = self.units.get(self.position) else {
            return Ok(Piece::End);
        };
        let two = self.units.get(self.position + 1).copied();
        let operator: Option<&'static str> = match unit {
            0x28 => Some("("),
            0x29 => Some(")"),
            0x2c => Some(","),
            0x3d => {
                if two == Some(0x3d) {
                    Some("==")
                } else {
                    Some("=")
                }
            }
            0x21 => {
                if two == Some(0x3d) {
                    Some("!=")
                } else {
                    Some("!")
                }
            }
            0x7c => {
                if two == Some(0x7c) {
                    Some("||")
                } else {
                    Some("|")
                }
            }
            0x26 => {
                if two == Some(0x26) {
                    Some("&&")
                } else {
                    Some("&")
                }
            }
            0x5e => Some("^"),
            0x2b => Some("+"),
            0x2d => Some("-"),
            0x2a => Some("*"),
            0x2f => Some("/"),
            0x25 => Some("%"),
            0x3c => {
                if two == Some(0x3d) {
                    Some("<=")
                } else {
                    Some("<")
                }
            }
            0x3e => {
                if two == Some(0x3d) {
                    Some(">=")
                } else {
                    Some(">")
                }
            }
            _ => None,
        };
        if let Some(operator) = operator {
            self.position += if operator.len() == 2 { 2 } else { 1 };
            return Ok(Piece::Operator(operator));
        }
        if is_digit(unit) {
            let mut cursor = Cursor::new(self.units[self.position..].to_vec());
            let Some(literal) = cursor.scan_number() else {
                return Err(ExpressionError::Syntax);
            };
            self.position += cursor.position();
            return match literal {
                Literal::Integer(value) => Ok(Piece::Number(value as i32)),
                Literal::Real(value) => Ok(Piece::Number(value as i32)),
                _ => Err(ExpressionError::Syntax),
            };
        }
        if is_letter(unit) || unit == u16::from(b'_') {
            let start = self.position;
            while let Some(&unit) = self.units.get(self.position) {
                if is_letter(unit) || is_digit(unit) || unit == u16::from(b'_') {
                    self.position += 1;
                } else {
                    break;
                }
            }
            let name = String::from_utf16_lossy(&self.units[start..self.position]);
            return Ok(Piece::Symbol(name));
        }
        Err(ExpressionError::Syntax)
    }

    /// The next piece of the expression, without consuming it.
    fn peek(&mut self) -> Result<Piece, ExpressionError> {
        let position = self.position;
        let piece = self.next()?;
        self.position = position;
        Ok(piece)
    }

    /// The operator that follows, if there is one.
    fn peek_operator(&mut self) -> Result<Option<&'static str>, ExpressionError> {
        match self.peek()? {
            Piece::Operator(operator) => Ok(Some(operator)),
            _ => Ok(None),
        }
    }

    /// An expression, down to the operators that bind looser than the
    /// given level.
    fn parse_binary(&mut self, level: u8) -> Result<Operand, ExpressionError> {
        let mut left = self.parse_unary()?;
        while let Some(operator) = self.peek_operator()? {
            let Some(operator_level) = precedence(operator) else {
                break;
            };
            if operator_level < level {
                break;
            }
            self.next()?;
            if operator == "=" {
                let Some(name) = left.symbol.clone() else {
                    return Err(ExpressionError::Syntax);
                };
                let right = self.parse_binary(operator_level + 1)?;
                self.values.set(&name, right.value);
                left = Operand {
                    value: right.value,
                    symbol: None,
                };
            } else {
                let right = self.parse_binary(operator_level + 1)?;
                left = Operand {
                    value: apply(operator, left.value, right.value)?,
                    symbol: None,
                };
            }
        }
        Ok(left)
    }

    /// A unary expression.
    fn parse_unary(&mut self) -> Result<Operand, ExpressionError> {
        match self.peek_operator()? {
            Some("!") => {
                self.next()?;
                let operand = self.parse_unary()?;
                Ok(Operand {
                    value: i32::from(operand.value == 0),
                    symbol: None,
                })
            }
            Some("+") => {
                self.next()?;
                let operand = self.parse_unary()?;
                Ok(Operand {
                    value: operand.value,
                    symbol: None,
                })
            }
            Some("-") => {
                self.next()?;
                let operand = self.parse_unary()?;
                Ok(Operand {
                    value: operand.value.wrapping_neg(),
                    symbol: None,
                })
            }
            _ => self.parse_primary(),
        }
    }

    /// A number, a name or a parenthesised expression.
    fn parse_primary(&mut self) -> Result<Operand, ExpressionError> {
        match self.next()? {
            Piece::Number(value) => Ok(Operand {
                value,
                symbol: None,
            }),
            Piece::Symbol(name) => {
                let value = self.values.get(&name);
                Ok(Operand {
                    value,
                    symbol: Some(name),
                })
            }
            Piece::Operator("(") => {
                let operand = self.parse_binary(1)?;
                match self.next()? {
                    Piece::Operator(")") => Ok(Operand {
                        value: operand.value,
                        symbol: None,
                    }),
                    _ => Err(ExpressionError::Syntax),
                }
            }
            _ => Err(ExpressionError::Syntax),
        }
    }
}

/// The level an operator binds at. A lower level binds looser.
fn precedence(operator: &str) -> Option<u8> {
    Some(match operator {
        "," => 1,
        "||" => 2,
        "&&" => 3,
        "|" => 4,
        "^" => 5,
        "&" => 6,
        "=" => 7,
        "!=" | "==" => 8,
        "<" | ">" | "<=" | ">=" => 9,
        "+" | "-" => 10,
        "%" | "/" | "*" => 11,
        _ => return None,
    })
}

/// The value of an operation on two operands.
fn apply(operator: &str, left: i32, right: i32) -> Result<i32, ExpressionError> {
    Ok(match operator {
        "," => right,
        "||" => i32::from(left != 0 || right != 0),
        "&&" => i32::from(left != 0 && right != 0),
        "|" => left | right,
        "^" => left ^ right,
        "&" => left & right,
        "!=" => i32::from(left != right),
        "==" => i32::from(left == right),
        "<" => i32::from(left < right),
        ">" => i32::from(left > right),
        "<=" => i32::from(left <= right),
        ">=" => i32::from(left >= right),
        "+" => left.wrapping_add(right),
        "-" => left.wrapping_sub(right),
        "*" => left.wrapping_mul(right),
        "/" => left
            .checked_div(right)
            .ok_or(ExpressionError::DivisionByZero)?,
        "%" => left
            .checked_rem(right)
            .ok_or(ExpressionError::DivisionByZero)?,
        _ => return Err(ExpressionError::Syntax),
    })
}
