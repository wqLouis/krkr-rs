//! The lexical analyser: script text in, tokens out.
//!
//! The analyser reads a script as a sequence of code units and produces the
//! token stream the parser consumes. A token is a kind and, for the kinds
//! that carry one, a literal value.
//!
//! The analyser is driven by the parser: two notifications tell it what the
//! parser expects next — a regular expression literal, or a name that is
//! not a reserved word. Everything else the analyser decides on its own.
//!
//! The source positions the analyser reports are counted in code units from
//! the start of the script, so a position names the same place in the text
//! the parser and the error messages name.

#![deny(unsafe_code)]

mod error;
mod literal;
mod number;
mod preprocess;
mod token;
mod value;

use std::collections::VecDeque;

pub use error::LexError;
pub use preprocess::PpValues;
pub use token::{Token, TokenKind};
pub use value::ValueRef;

pub(crate) use value::Value;

use literal::StringEnd;
pub(crate) use number::engine_real;

/// The code unit that stands in for the carriage return of a newline pair.
///
/// A newline pair is one line break, but the carriage return and the line
/// feed are two code units, and a position counts code units. The carriage
/// return becomes this marker, which every scan steps over, so the line
/// feed is the only code unit that ends a line.
pub(crate) const SKIP_CODE: u16 = 0xffff;

/// A cursor over the code units of a script.
///
/// The cursor knows the two rules every scan shares: stepping over the
/// newline marker, and stepping over the whitespace.
pub(crate) struct Cursor {
    text: Vec<u16>,
    position: usize,
}

impl Cursor {
    /// A cursor over the given code units.
    pub fn new(text: Vec<u16>) -> Self {
        Self { text, position: 0 }
    }

    /// The position of the cursor, counted in code units.
    pub fn position(&self) -> usize {
        self.position
    }

    /// Move the cursor to a position.
    pub fn move_to(&mut self, position: usize) {
        self.position = position;
    }

    /// The code unit at the cursor.
    pub fn current(&self) -> Option<u16> {
        self.text.get(self.position).copied()
    }

    /// The code unit `offset` code units after the cursor.
    pub fn ahead(&self, offset: usize) -> Option<u16> {
        self.text.get(self.position + offset).copied()
    }

    /// The code unit at an absolute position.
    pub fn unit_at(&self, position: usize) -> Option<u16> {
        self.text.get(position).copied()
    }

    /// The code units of the script.
    pub fn units(&self) -> &[u16] {
        &self.text
    }

    /// Move to the next code unit, stepping over the newline markers.
    ///
    /// Returns whether there is a code unit left to read.
    pub fn advance(&mut self) -> bool {
        self.position += 1;
        if self.current() == Some(SKIP_CODE) {
            while self.current() == Some(SKIP_CODE) {
                self.position += 1;
            }
        }
        self.current().is_some()
    }

    /// Move forward by the given number of code units.
    pub fn advance_by(&mut self, count: usize) {
        self.position += count;
    }

    /// Step over the whitespace and the newline markers.
    ///
    /// Returns whether there is a code unit left to read.
    pub fn skip_space(&mut self) -> bool {
        self.position = self.skip_space_at(self.position);
        self.current().is_some()
    }

    /// The position after the whitespace at a position.
    ///
    /// The scan is written as a search for the first code unit that is not
    /// whitespace, so the compiler can vectorise it.
    pub fn skip_space_at(&self, position: usize) -> usize {
        let units = &self.text[position.min(self.text.len())..];
        let length = units
            .iter()
            .position(|&unit| !is_space(unit) && unit != SKIP_CODE)
            .unwrap_or(units.len());
        position + length
    }

    /// The word at the current position, if it is there.
    ///
    /// The word is matched exactly, and it must not run into a name: the
    /// word `in` is not the word `instanceof`.
    pub fn match_word(&mut self, word: &str) -> bool {
        let start = self.position;
        for expected in word.bytes() {
            if self.current() != Some(u16::from(expected)) {
                self.move_to(start);
                return false;
            }
            self.advance();
        }
        if let Some(unit) = self.current()
            && (is_letter(unit) || unit == u16::from(b'_'))
        {
            self.move_to(start);
            return false;
        }
        true
    }

    /// Step over a comment, if there is one at the cursor.
    pub fn skip_comment(&mut self) -> Comment {
        if self.current() != Some(u16::from(b'/')) {
            return Comment::Absent;
        }
        match self.ahead(1) {
            Some(unit) if unit == u16::from(b'/') => {
                // a line comment, up to the line feed
                while let Some(unit) = self.current() {
                    if unit == u16::from(b'\n') {
                        break;
                    }
                    if !self.advance() {
                        break;
                    }
                }
                if self.current().is_none() {
                    return Comment::Ended;
                }
                self.position += 1;
                self.skip_space();
                if self.current().is_none() {
                    return Comment::Ended;
                }
                Comment::Skipped
            }
            Some(unit) if unit == u16::from(b'*') => {
                // a block comment, which may nest
                self.position += 2;
                if self.current().is_none() {
                    return Comment::Unterminated;
                }
                let mut level = 0;
                loop {
                    if self.current() == Some(u16::from(b'/'))
                        && self.ahead(1) == Some(u16::from(b'*'))
                    {
                        level += 1;
                    }
                    if self.current() == Some(u16::from(b'*'))
                        && self.ahead(1) == Some(u16::from(b'/'))
                    {
                        if level == 0 {
                            self.position += 2;
                            break;
                        }
                        level -= 1;
                    }
                    if !self.advance() {
                        return Comment::Unterminated;
                    }
                }
                if self.current().is_none() {
                    return Comment::Ended;
                }
                self.skip_space();
                if self.current().is_none() {
                    return Comment::Ended;
                }
                Comment::Skipped
            }
            _ => Comment::Absent,
        }
    }
}

/// What a comment scan found.
pub(crate) enum Comment {
    /// A comment was stepped over.
    Skipped,
    /// The script ended inside or after a comment.
    Ended,
    /// A block comment was not terminated.
    Unterminated,
    /// There was no comment.
    Absent,
}

/// Whether the code unit is a space.
///
/// Only the ASCII spaces are spaces: a code unit outside the ASCII range is
/// a character, not a space.
///
/// The test is written without a branch so that a loop over code units
/// vectorises: the comparisons become lane masks.
pub(crate) fn is_space(unit: u16) -> bool {
    let control = unit.wrapping_sub(0x09) < 5;
    let space = unit == 0x20;
    unit < 0x100 && (control || space)
}

/// Whether the code unit is a decimal digit.
pub(crate) fn is_digit(unit: u16) -> bool {
    matches!(unit, 0x30..=0x39)
}

/// Whether the code unit is a letter.
///
/// Every code unit outside the ASCII range is a letter: the script may hold
/// any character, and a character that is not a digit, a space or a
/// punctuation mark is part of a name.
pub(crate) fn is_letter(unit: u16) -> bool {
    unit >= 0x100 || matches!(unit, 0x41..=0x5a | 0x61..=0x7a)
}

/// Whether the code unit can be part of a name.
///
/// A code unit outside the ASCII range is a letter: the script may hold any
/// character, and a character that is not a digit, a space or a punctuation
/// mark is part of a name.
///
/// The test is written without a branch so that a loop over code units
/// vectorises: the comparisons become lane masks.
pub(crate) fn is_name_unit(unit: u16) -> bool {
    let digit = unit.wrapping_sub(0x30) < 10;
    let letter = (unit | 0x20).wrapping_sub(0x61) < 26;
    let underscore = unit == 0x5f;
    let wide = unit >= 0x100;
    digit || letter || underscore || wide
}

/// The digit sets a number literal can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Digits {
    /// The decimal digits.
    Decimal,
    /// The hexadecimal digits.
    Hexadecimal,
    /// The octal digits.
    Octal,
    /// The binary digits.
    Binary,
}

impl Digits {
    /// The value of a digit, or `None` when the code unit is not a digit of
    /// this set.
    pub fn value(self, unit: u16) -> Option<i32> {
        match self {
            Self::Decimal => match unit {
                0x30..=0x39 => Some(i32::from(unit - 0x30)),
                _ => None,
            },
            Self::Hexadecimal => match unit {
                0x30..=0x39 => Some(i32::from(unit - 0x30)),
                0x41..=0x46 => Some(i32::from(unit - 0x41 + 10)),
                0x61..=0x66 => Some(i32::from(unit - 0x61 + 10)),
                _ => None,
            },
            Self::Octal => match unit {
                0x30..=0x37 => Some(i32::from(unit - 0x30)),
                _ => None,
            },
            Self::Binary => match unit {
                0x30 => Some(0),
                0x31 => Some(1),
                _ => None,
            },
        }
    }

    /// The number of bits one digit of this set carries.
    pub fn bits(self) -> u32 {
        match self {
            Self::Decimal | Self::Hexadecimal => 4,
            Self::Octal => 3,
            Self::Binary => 1,
        }
    }
}

/// What a scan produced.
enum Scan {
    /// A token.
    Token(Token),
    /// Nothing yet: the caller asks again.
    Pending,
    /// The end of the script.
    End,
}

/// The state of an embeddable expression — a string literal that holds
/// script code.
struct Embeddable {
    /// What the analyser expects next.
    state: EmbeddableState,
    /// The nesting level the embedded expression started at.
    nesting: i32,
    /// The token that ends the embedded expression.
    waiting: TokenKind,
    /// The delimiter of the string literal.
    delimiter: u16,
    /// Whether a plus sign is needed before the next part.
    needs_plus: bool,
}

/// What an embeddable expression expects next.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EmbeddableState {
    /// The opening parenthesis.
    Start,
    /// The next part of the string literal.
    StringLiteral,
    /// The embedded expression.
    Expression,
}

/// The lexical analyser.
///
/// The analyser owns the script it reads and the tokens it has produced but
/// not yet handed out.
pub struct Lexer {
    cursor: Cursor,
    /// The position the last token started at.
    previous_position: usize,
    /// The first token has not been read yet.
    first: bool,
    /// The script is an expression, not a program.
    expression: bool,
    /// The expression's value is wanted.
    result_needed: bool,
    /// The nesting level of the brackets.
    nesting: i32,
    /// The next token is a regular expression literal.
    regular_expression: bool,
    /// The next name is not a reserved word.
    bare_word: bool,
    /// The tokens that were produced but not handed out yet.
    pending: VecDeque<Token>,
    /// The embeddable expressions that are open.
    embeddable: Vec<Embeddable>,
    /// The nesting level of the conditional-compilation statements.
    conditional_level: i32,
    /// The values the conditional-compilation statements read and write.
    conditional_values: PpValues,
    /// Whether the script used a conditional-compilation statement.
    ///
    /// The flag makes the script block non-reusable, because the values
    /// the statement set belong to that one script.
    using_preprocessor: bool,
    /// The values the tokens name.
    ///
    /// The string and the binary data live in the two buffers below, so a
    /// value costs one entry here and no allocation.
    ///
    /// The buffers are bounded by the script: every code unit of the text
    /// is copied into the string buffer at most once, and every value is
    /// one entry. They are released with the analyser, at the end of the
    /// script.
    values: Vec<Value>,
    /// The code units of every string value.
    strings: Vec<u16>,
    /// The bytes of every binary value.
    octets: Vec<u8>,
}

impl Lexer {
    /// An analyser for a script.
    ///
    /// The script is a sequence of code units; a code unit that is not a
    /// character of the text ends the script.
    pub fn new(script: &[u16]) -> Self {
        Self::with_text(script.to_vec(), false, false)
    }

    /// An analyser for an expression.
    ///
    /// The expression is read as a program whose last statement is
    /// terminated, and the analyser produces the token that returns the
    /// expression's value when `result_needed` is set.
    pub fn expression(script: &[u16], result_needed: bool) -> Self {
        Self::with_text(script.to_vec(), true, result_needed)
    }

    fn with_text(mut text: Vec<u16>, expression: bool, result_needed: bool) -> Self {
        if let Some(end) = text.iter().position(|&unit| unit == 0) {
            text.truncate(end);
        }
        unify_newlines(&mut text);
        if !expression
            && text.first() == Some(&u16::from(b'#'))
            && text.get(1) == Some(&u16::from(b'!'))
        {
            // a script that starts with a shebang is a comment
            text[0] = u16::from(b'/');
            text[1] = u16::from(b'/');
        }
        if expression {
            text.push(u16::from(b';'));
        }
        Self {
            cursor: Cursor::new(text),
            previous_position: 0,
            first: true,
            expression,
            result_needed,
            nesting: 0,
            regular_expression: false,
            bare_word: false,
            pending: VecDeque::new(),
            embeddable: Vec::new(),
            conditional_level: 0,
            conditional_values: PpValues::new(),
            using_preprocessor: false,
            values: Vec::new(),
            strings: Vec::new(),
            octets: Vec::new(),
        }
    }

    /// The value of a token.
    ///
    /// The value is borrowed from the analyser's buffers, so it lives as
    /// long as the analyser does: a token names its value by its index, and
    /// the analyser keeps the value. The tokens of a whole script therefore
    /// cost eight bytes each, and a token that carries a name costs no
    /// allocation.
    pub fn value(&self, token: &Token) -> Option<ValueRef<'_>> {
        token.value.map(|index| self.value_at(index))
    }

    /// The value at an index in the analyser's values.
    pub(crate) fn value_at(&self, index: u32) -> ValueRef<'_> {
        match self.values[index as usize] {
            Value::Null => ValueRef::Null,
            Value::Integer(value) => ValueRef::Integer(value),
            Value::Real(value) => ValueRef::Real(value),
            Value::String { offset, len } => {
                let start = offset as usize;
                ValueRef::String(&self.strings[start..start + len as usize])
            }
            Value::Octet { offset, len } => {
                let start = offset as usize;
                ValueRef::Octet(&self.octets[start..start + len as usize])
            }
        }
    }

    /// Keep a value and name it.
    fn push_value(&mut self, value: Value) -> u32 {
        self.values.push(value);
        (self.values.len() - 1) as u32
    }

    /// Keep the code units of a string and name the value.
    fn push_string(&mut self, units: &[u16]) -> u32 {
        let offset = self.strings.len() as u32;
        self.strings.extend_from_slice(units);
        let len = units.len() as u32;
        self.push_value(Value::String { offset, len })
    }

    /// Keep the text of a name and name the value.
    ///
    /// The newline markers are not part of the name, so they do not go into
    /// the buffer.
    fn push_name(&mut self, start: usize, end: usize) -> u32 {
        let offset = self.strings.len() as u32;
        for &unit in &self.cursor.units()[start..end] {
            if unit != SKIP_CODE {
                self.strings.push(unit);
            }
        }
        let len = self.strings.len() as u32 - offset;
        self.push_value(Value::String { offset, len })
    }

    /// Keep the bytes of a binary value and name the value.
    fn push_octet(&mut self, bytes: &[u8]) -> u32 {
        let offset = self.octets.len() as u32;
        self.octets.extend_from_slice(bytes);
        let len = bytes.len() as u32;
        self.push_value(Value::Octet { offset, len })
    }

    /// The values the conditional-compilation statements read and write.
    ///
    /// The values outlive a script: a value one script sets is visible in
    /// the next one, so the owner of the engine moves them from one
    /// analyser to the next.
    pub fn conditional_values(&self) -> &PpValues {
        &self.conditional_values
    }

    /// The values the conditional-compilation statements read and write.
    pub fn conditional_values_mut(&mut self) -> &mut PpValues {
        &mut self.conditional_values
    }

    /// Whether the script used a conditional-compilation statement.
    ///
    /// The flag makes the script block non-reusable, because the values
    /// the statement set belong to that one script.
    pub fn using_preprocessor(&self) -> bool {
        self.using_preprocessor
    }

    /// The next token, or `None` at the end of the script.
    ///
    /// # Errors
    ///
    /// A malformed literal, a malformed number, an unterminated comment and
    /// a malformed conditional-compilation statement are reported as a
    /// [`LexError`]; the analyser stops at the failure.
    pub fn next_token(&mut self) -> Result<Option<Token>, LexError> {
        if self.first {
            self.first = false;
            self.cursor.move_to(0);
            if self.expression && self.result_needed {
                return Ok(Some(Token::new(TokenKind::Return)));
            }
        }
        loop {
            if let Some(token) = self.pending.pop_front() {
                return Ok(Some(token));
            }
            // The analyser times its own token production: the measurement
            // is off by default, and a token that is not measured costs one
            // relaxed atomic load and nothing else.
            let scan = crate::vm::stats::timed_lexer(|| -> Result<Scan, LexError> {
                if self.embeddable.is_empty() {
                    self.cursor.skip_space();
                    self.scan_token()
                } else {
                    self.scan_embeddable()
                }
            })?;
            match scan {
                Scan::Token(token) => return Ok(Some(token)),
                Scan::Pending => continue,
                Scan::End => {
                    if self.conditional_level != 0 {
                        return Err(LexError::ConditionalCompilation {
                            position: self.cursor.position(),
                        });
                    }
                    return Ok(None);
                }
            }
        }
    }

    /// The position in the script the analyser has read up to, counted in
    /// code units.
    pub fn current_position(&self) -> usize {
        self.cursor.position()
    }

    /// Tell the analyser that the next token is a regular expression
    /// literal.
    ///
    /// The parser decides this: a slash that starts a factor is a regular
    /// expression, a slash that divides is an operator. The analyser reads
    /// the literal from the slash, so the call must come before the next
    /// token is read.
    pub fn expect_regexp(&mut self) {
        self.regular_expression = true;
    }

    /// Tell the analyser that the next name is an ordinary name, not a
    /// reserved word.
    ///
    /// The parser decides this: a name that follows a dot is a member name
    /// whatever it is written like. The call must come before the next
    /// token is read.
    pub fn expect_bare_word(&mut self) {
        self.bare_word = true;
    }

    /// The token at the current position.
    fn scan_token(&mut self) -> Result<Scan, LexError> {
        if self.regular_expression {
            self.regular_expression = false;
            self.cursor.move_to(self.previous_position);
            self.cursor.advance();
            let pattern = self.cursor.scan_regexp()?;
            let value = self.push_string(pattern.units());
            return Ok(Scan::Token(Token::with_value(TokenKind::Regexp, value)));
        }

        loop {
            self.previous_position = self.cursor.position();
            let Some(character) = self.cursor.current() else {
                return Ok(Scan::End);
            };
            if character == u16::from(b'/') {
                match self.cursor.skip_comment() {
                    Comment::Skipped => continue,
                    Comment::Ended => return Ok(Scan::End),
                    Comment::Unterminated => return Err(self.unterminated_comment()),
                    Comment::Absent => {}
                }
            }
            if character == u16::from(b'.') && self.cursor.ahead(1).is_some_and(is_digit) {
                let Some(value) = self.cursor.scan_number() else {
                    return Err(self.number_error());
                };
                let value = self.push_value(value);
                return Ok(Scan::Token(Token::with_value(TokenKind::ConstVal, value)));
            }
            if let Some(kind) = self.match_operator() {
                return Ok(Scan::Token(Token::new(kind)));
            }
            match character {
                unit if unit == u16::from(b'.') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Dot)));
                }
                unit if unit == u16::from(b'<') => {
                    if self.cursor.ahead(1) == Some(u16::from(b'%')) {
                        let bytes = self.cursor.scan_octet()?;
                        let value = self.push_octet(&bytes);
                        return Ok(Scan::Token(Token::with_value(TokenKind::ConstVal, value)));
                    }
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::LessThan)));
                }
                unit if unit == u16::from(b'/') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Slash)));
                }
                unit if unit == u16::from(b'[') => {
                    self.nesting += 1;
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::LeftBracket)));
                }
                unit if unit == u16::from(b']') => {
                    self.nesting -= 1;
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::RightBracket)));
                }
                unit if unit == u16::from(b'(') => {
                    self.nesting += 1;
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::LeftParenthesis)));
                }
                unit if unit == u16::from(b')') => {
                    self.nesting -= 1;
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::RightParenthesis)));
                }
                unit if unit == u16::from(b'{') => {
                    self.nesting += 1;
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::LeftBrace)));
                }
                unit if unit == u16::from(b'}') => {
                    self.nesting -= 1;
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::RightBrace)));
                }
                unit if unit == u16::from(b'~') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Tilde)));
                }
                unit if unit == u16::from(b'?') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Question)));
                }
                unit if unit == u16::from(b':') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Colon)));
                }
                unit if unit == u16::from(b',') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Comma)));
                }
                unit if unit == u16::from(b';') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Semicolon)));
                }
                unit if unit == u16::from(b'#') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Sharp)));
                }
                unit if unit == u16::from(b'$') => {
                    self.cursor.advance();
                    return Ok(Scan::Token(Token::new(TokenKind::Dollar)));
                }
                unit if unit == u16::from(b'\'') || unit == u16::from(b'"') => {
                    let scan = self.cursor.scan_string(unit, false)?;
                    let value = self.push_string(scan.text.units());
                    return Ok(Scan::Token(Token::with_value(TokenKind::ConstVal, value)));
                }
                unit if unit == u16::from(b'@') => match self.scan_at()? {
                    At::Embeddable => return Ok(Scan::Pending),
                    At::Continue => continue,
                    At::End => return Ok(Scan::End),
                    At::Invalid => return Err(self.invalid_character(character)),
                },
                unit if is_digit(unit) => {
                    let Some(value) = self.cursor.scan_number() else {
                        return Err(self.number_error());
                    };
                    let value = self.push_value(value);
                    return Ok(Scan::Token(Token::with_value(TokenKind::ConstVal, value)));
                }
                _ => {
                    if let Some(kind) = single_operator(character) {
                        self.cursor.advance();
                        return Ok(Scan::Token(Token::new(kind)));
                    }
                    if !is_letter(character) && character != u16::from(b'_') {
                        return Err(self.invalid_character(character));
                    }
                    let start = self.cursor.position();
                    let end = self.scan_name();
                    let kind = if self.bare_word {
                        None
                    } else {
                        reserved_word(&self.cursor.units()[start..end])
                    };
                    self.bare_word = false;
                    return Ok(Scan::Token(match kind {
                        Some(TokenKind::False) => {
                            let value = self.push_value(Value::Integer(0));
                            Token::with_value(TokenKind::ConstVal, value)
                        }
                        Some(TokenKind::True) => {
                            let value = self.push_value(Value::Integer(1));
                            Token::with_value(TokenKind::ConstVal, value)
                        }
                        Some(TokenKind::Null) => {
                            let value = self.push_value(Value::Null);
                            Token::with_value(TokenKind::ConstVal, value)
                        }
                        Some(TokenKind::NaN) => {
                            let value = self.push_value(Value::Real(f64::NAN));
                            Token::with_value(TokenKind::ConstVal, value)
                        }
                        Some(TokenKind::Infinity) => {
                            let value = self.push_value(Value::Real(f64::INFINITY));
                            Token::with_value(TokenKind::ConstVal, value)
                        }
                        Some(kind) => Token::new(kind),
                        None => {
                            let value = self.push_name(start, end);
                            Token::with_value(TokenKind::Symbol, value)
                        }
                    }));
                }
            }
        }
    }

    /// The operator at the current position, if there is one.
    ///
    /// The table is the longest match over the text at the cursor. A
    /// newline marker inside the operator is stepped over while the
    /// operator is matched, so an operator written across a newline pair is
    /// one operator.
    fn match_operator(&mut self) -> Option<TokenKind> {
        // The operator characters are ASCII. A code unit that is not one --
        // a newline marker, or a character of the script -- can hide a
        // longer operator behind it, so the window goes to the slow path.
        let first = self.cursor.current()?;
        let second = self.cursor.ahead(1);
        let third = self.cursor.ahead(2);
        let fourth = self.cursor.ahead(3);
        let ascii = |unit: Option<u16>| unit.is_some_and(|unit| unit < 0x80);
        if first < 0x80 && ascii(second) && ascii(third) && ascii(fourth) {
            let window = [
                first as u8,
                second.unwrap_or(0) as u8,
                third.unwrap_or(0) as u8,
                fourth.unwrap_or(0) as u8,
            ];
            if let Some((kind, length)) = operator_kind(&window) {
                if length == 4 && self.cursor.ahead(4) == Some(SKIP_CODE) {
                    // a marker right after the operator means a longer
                    // operator may be hidden behind it
                    return self.match_operator_with_markers();
                }
                self.cursor.advance_by(length);
                return Some(kind);
            }
            return None;
        }
        self.match_operator_with_markers()
    }

    /// The operator at the current position, when a newline marker stands
    /// inside it.
    ///
    /// The window holds the characters of the operator without the markers,
    /// and the number of code units each of them stands for.
    fn match_operator_with_markers(&mut self) -> Option<TokenKind> {
        // the characters of the operator, without the markers
        let mut window = [0u8; 5];
        let mut count = 0;
        let mut position = self.cursor.position();
        while count < window.len() {
            let Some(unit) = self.cursor.unit_at(position) else {
                break;
            };
            if unit == SKIP_CODE {
                position += 1;
                continue;
            }
            if unit >= 0x80 {
                break;
            }
            window[count] = unit as u8;
            count += 1;
            position += 1;
        }
        let (kind, length) = operator_kind(&window[..count])?;
        // the matched characters, and the markers that follow the last of
        // them, are the code units the operator stands for
        let mut units = 0;
        let mut position = self.cursor.position();
        let mut characters = 0;
        while characters < length {
            let Some(unit) = self.cursor.unit_at(position) else {
                break;
            };
            units += 1;
            position += 1;
            if unit != SKIP_CODE {
                characters += 1;
            }
        }
        while self.cursor.unit_at(position) == Some(SKIP_CODE) {
            units += 1;
            position += 1;
        }
        self.cursor.move_to(self.cursor.position() + units);
        Some(kind)
    }

    /// The name at the current position.
    ///
    /// The name is the run of code units that can be part of a name. The
    /// newline markers inside it are not part of the name.
    fn scan_name(&mut self) -> usize {
        let units = self.cursor.units();
        let start = self.cursor.position();
        let rest = &units[start..];
        let mut length = 0;
        while length < rest.len() {
            let unit = rest[length];
            if !is_name_unit(unit) {
                break;
            }
            length += 1;
        }
        let end = start + length;
        self.cursor.move_to(end);
        end
    }

    /// What an `@` at the current position means.
    fn scan_at(&mut self) -> Result<At, LexError> {
        let start = self.cursor.position();
        if !self.cursor.advance() {
            return Ok(At::End);
        }
        self.cursor.skip_space();
        let Some(character) = self.cursor.current() else {
            return Ok(At::End);
        };
        if character == u16::from(b'\'') || character == u16::from(b'"') {
            self.embeddable.push(Embeddable {
                state: EmbeddableState::Start,
                nesting: self.nesting,
                waiting: TokenKind::Semicolon,
                delimiter: character,
                needs_plus: false,
            });
            self.cursor.advance();
            return Ok(At::Embeddable);
        }
        self.cursor.move_to(start);
        let statement = self.scan_conditional_statement()?;
        match statement {
            Statement::Continue => {
                // the script used a conditional-compilation statement
                self.using_preprocessor = true;
                Ok(At::Continue)
            }
            Statement::End => {
                self.using_preprocessor = true;
                Ok(At::End)
            }
            Statement::Absent => Ok(At::Invalid),
        }
    }

    /// The next token of an embeddable expression.
    fn scan_embeddable(&mut self) -> Result<Scan, LexError> {
        let Some(state) = self.embeddable.last() else {
            return Ok(Scan::End);
        };
        match state.state {
            EmbeddableState::Start => {
                self.pending
                    .push_back(Token::new(TokenKind::LeftParenthesis));
                if let Some(state) = self.embeddable.last_mut() {
                    state.state = EmbeddableState::StringLiteral;
                }
                Ok(Scan::Pending)
            }
            EmbeddableState::StringLiteral => {
                let (delimiter, mut needs_plus) = {
                    let state = self.embeddable.last().unwrap_or(&Embeddable {
                        state: EmbeddableState::StringLiteral,
                        nesting: 0,
                        waiting: TokenKind::Semicolon,
                        delimiter: 0,
                        needs_plus: false,
                    });
                    (state.delimiter, state.needs_plus)
                };
                let scan = self.cursor.scan_string_content(delimiter, true)?;
                match scan.end {
                    StringEnd::Delimiter => {
                        if !scan.text.is_empty() && needs_plus {
                            self.pending.push_back(Token::new(TokenKind::Plus));
                        }
                        if !scan.text.is_empty() || !needs_plus {
                            let value = self.push_string(scan.text.units());
                            self.pending
                                .push_back(Token::with_value(TokenKind::ConstVal, value));
                        }
                        self.pending
                            .push_back(Token::new(TokenKind::RightParenthesis));
                        self.embeddable.pop();
                        Ok(Scan::Pending)
                    }
                    _ => {
                        if !scan.text.is_empty() {
                            if needs_plus {
                                self.pending.push_back(Token::new(TokenKind::Plus));
                            }
                            let value = self.push_string(scan.text.units());
                            self.pending
                                .push_back(Token::with_value(TokenKind::ConstVal, value));
                            needs_plus = true;
                        }
                        if needs_plus {
                            self.pending.push_back(Token::new(TokenKind::Plus));
                        }
                        self.pending.push_back(Token::new(TokenKind::String));
                        self.pending
                            .push_back(Token::new(TokenKind::LeftParenthesis));
                        let waiting = match scan.end {
                            StringEnd::Ampersand => TokenKind::Semicolon,
                            _ => {
                                self.nesting += 1;
                                TokenKind::RightBrace
                            }
                        };
                        if let Some(state) = self.embeddable.last_mut() {
                            state.state = EmbeddableState::Expression;
                            state.waiting = waiting;
                            state.needs_plus = needs_plus;
                        }
                        Ok(Scan::Pending)
                    }
                }
            }
            EmbeddableState::Expression => {
                self.cursor.skip_space();
                let scan = self.scan_token()?;
                if let Scan::Token(token) = &scan {
                    let state = self.embeddable.last();
                    let waiting = state.map(|state| state.waiting);
                    let nesting = state.map(|state| state.nesting);
                    if Some(token.kind) == waiting && nesting == Some(self.nesting) {
                        self.pending
                            .push_back(Token::new(TokenKind::RightParenthesis));
                        if let Some(state) = self.embeddable.last_mut() {
                            state.needs_plus = true;
                            state.state = EmbeddableState::StringLiteral;
                        }
                        return Ok(Scan::Pending);
                    }
                }
                Ok(scan)
            }
        }
    }

    /// The error for a character that cannot begin a token.
    fn invalid_character(&self, character: u16) -> LexError {
        LexError::InvalidCharacter {
            position: self.cursor.position(),
            character: error::escape_characters(&[character]),
        }
    }

    /// The error for a comment that is not terminated.
    fn unterminated_comment(&self) -> LexError {
        LexError::UnterminatedComment {
            position: self.cursor.position(),
        }
    }

    /// The error for a number that cannot be parsed.
    fn number_error(&self) -> LexError {
        LexError::Number {
            position: self.cursor.position(),
        }
    }
}

/// What an `@` at the current position means.
enum At {
    /// An embeddable expression was opened.
    Embeddable,
    /// A conditional-compilation statement was handled; scan again.
    Continue,
    /// The script ended.
    End,
    /// The `@` is neither a statement nor an embeddable expression.
    Invalid,
}

/// What a conditional-compilation statement did.
pub(crate) enum Statement {
    /// The statement was handled; the scan continues.
    Continue,
    /// The script ended.
    End,
    /// The `@` does not start a statement.
    Absent,
}

/// Replace the carriage return of a newline pair with the newline marker.
///
/// A newline pair is one line break. The line feed stays, so the line
/// count is the line count of the text; the carriage return becomes the
/// marker, so a position counts the same number of code units before and
/// after the pair.
fn unify_newlines(text: &mut [u16]) {
    for index in 0..text.len() {
        if text[index] == u16::from(b'\r') {
            if text.get(index + 1) == Some(&u16::from(b'\n')) {
                text[index] = SKIP_CODE;
            } else {
                text[index] = u16::from(b'\n');
            }
        }
    }
}

/// The operator a single character stands for.
fn single_operator(character: u16) -> Option<TokenKind> {
    Some(match character {
        unit if unit == u16::from(b'>') => TokenKind::GreaterThan,
        unit if unit == u16::from(b'<') => TokenKind::LessThan,
        unit if unit == u16::from(b'=') => TokenKind::Equal,
        unit if unit == u16::from(b'!') => TokenKind::Exclamation,
        unit if unit == u16::from(b'&') => TokenKind::Ampersand,
        unit if unit == u16::from(b'|') => TokenKind::VerticalBar,
        unit if unit == u16::from(b'+') => TokenKind::Plus,
        unit if unit == u16::from(b'-') => TokenKind::Minus,
        unit if unit == u16::from(b'*') => TokenKind::Asterisk,
        unit if unit == u16::from(b'\\') => TokenKind::Backslash,
        unit if unit == u16::from(b'%') => TokenKind::Percent,
        unit if unit == u16::from(b'^') => TokenKind::Chevron,
        _ => return None,
    })
}

/// The token kind a reserved word stands for.
///
/// The reserved words are ASCII, so a name that holds another character is
/// not one. The newline markers are not part of the name, so they are
/// stepped over.
fn reserved_word(name: &[u16]) -> Option<TokenKind> {
    let mut bytes = [0u8; 16];
    let mut length = 0;
    for &unit in name {
        if unit == SKIP_CODE {
            continue;
        }
        if unit >= 0x80 || length == bytes.len() {
            return None;
        }
        bytes[length] = unit as u8;
        length += 1;
    }
    Some(match &bytes[..length] {
        b"break" => TokenKind::Break,
        b"continue" => TokenKind::Continue,
        b"const" => TokenKind::Const,
        b"catch" => TokenKind::Catch,
        b"class" => TokenKind::Class,
        b"case" => TokenKind::Case,
        b"debugger" => TokenKind::Debugger,
        b"default" => TokenKind::Default,
        b"delete" => TokenKind::Delete,
        b"do" => TokenKind::Do,
        b"extends" => TokenKind::Extends,
        b"export" => TokenKind::Export,
        b"enum" => TokenKind::Enum,
        b"else" => TokenKind::Else,
        b"function" => TokenKind::Function,
        b"finally" => TokenKind::Finally,
        b"false" => TokenKind::False,
        b"for" => TokenKind::For,
        b"global" => TokenKind::Global,
        b"getter" => TokenKind::Getter,
        b"goto" => TokenKind::Goto,
        b"incontextof" => TokenKind::InContextOf,
        b"invalidate" => TokenKind::Invalidate,
        b"instanceof" => TokenKind::InstanceOf,
        b"isvalid" => TokenKind::IsValid,
        b"import" => TokenKind::Import,
        b"int" => TokenKind::Int,
        b"in" => TokenKind::In,
        b"if" => TokenKind::If,
        b"null" => TokenKind::Null,
        b"new" => TokenKind::New,
        b"octet" => TokenKind::Octet,
        b"protected" => TokenKind::Protected,
        b"property" => TokenKind::Property,
        b"private" => TokenKind::Private,
        b"public" => TokenKind::Public,
        b"return" => TokenKind::Return,
        b"real" => TokenKind::Real,
        b"synchronized" => TokenKind::Synchronized,
        b"switch" => TokenKind::Switch,
        b"static" => TokenKind::Static,
        b"setter" => TokenKind::Setter,
        b"string" => TokenKind::String,
        b"super" => TokenKind::Super,
        b"typeof" => TokenKind::TypeOf,
        b"throw" => TokenKind::Throw,
        b"this" => TokenKind::This,
        b"true" => TokenKind::True,
        b"try" => TokenKind::Try,
        b"void" => TokenKind::Void,
        b"var" => TokenKind::Var,
        b"while" => TokenKind::While,
        b"NaN" => TokenKind::NaN,
        b"Infinity" => TokenKind::Infinity,
        b"with" => TokenKind::With,
        _ => return None,
    })
}

/// The operator the text starts, and how many characters it is.
///
/// The table is the longest match: the longer operators come first.
fn operator_kind(text: &[u8]) -> Option<(TokenKind, usize)> {
    Some(match text {
        [b'>', b'>', b'>', b'=', ..] => (TokenKind::RightBitShiftAssign, 4),
        [b'>', b'>', b'>', ..] => (TokenKind::RightBitShift, 3),
        [b'>', b'>', b'=', ..] => (TokenKind::RightArithmeticShiftAssign, 3),
        [b'>', b'>', ..] => (TokenKind::RightArithmeticShift, 2),
        [b'>', b'=', ..] => (TokenKind::GreaterOrEqual, 2),
        [b'<', b'<', b'=', ..] => (TokenKind::LeftBitShiftAssign, 3),
        [b'<', b'-', b'>', ..] => (TokenKind::Swap, 3),
        [b'<', b'=', ..] => (TokenKind::LessOrEqual, 2),
        [b'<', b'<', ..] => (TokenKind::LeftBitShift, 2),
        [b'=', b'=', b'=', ..] => (TokenKind::DiscriminantEqual, 3),
        [b'=', b'=', ..] => (TokenKind::EqualEqual, 2),
        [b'=', b'>', ..] => (TokenKind::Comma, 2),
        [b'!', b'=', b'=', ..] => (TokenKind::DiscriminantNotEqual, 3),
        [b'!', b'=', ..] => (TokenKind::NotEqual, 2),
        [b'&', b'&', b'=', ..] => (TokenKind::LogicalAndAssign, 3),
        [b'&', b'&', ..] => (TokenKind::LogicalAnd, 2),
        [b'&', b'=', ..] => (TokenKind::AmpersandAssign, 2),
        [b'|', b'|', b'=', ..] => (TokenKind::LogicalOrAssign, 3),
        [b'|', b'|', ..] => (TokenKind::LogicalOr, 2),
        [b'|', b'=', ..] => (TokenKind::VerticalBarAssign, 2),
        [b'.', b'.', b'.', ..] => (TokenKind::Omit, 3),
        [b'+', b'+', ..] => (TokenKind::Increment, 2),
        [b'+', b'=', ..] => (TokenKind::PlusAssign, 2),
        [b'-', b'=', ..] => (TokenKind::MinusAssign, 2),
        [b'-', b'-', ..] => (TokenKind::Decrement, 2),
        [b'*', b'=', ..] => (TokenKind::AsteriskAssign, 2),
        [b'/', b'=', ..] => (TokenKind::SlashAssign, 2),
        [b'\\', b'=', ..] => (TokenKind::BackslashAssign, 2),
        [b'%', b'=', ..] => (TokenKind::PercentAssign, 2),
        [b'^', b'=', ..] => (TokenKind::ChevronAssign, 2),
        _ => return None,
    })
}
