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

mod error;
mod literal;
mod number;
mod preprocess;
mod scaffold;
mod token;
mod value;

use std::collections::VecDeque;

use logos::Logos;

pub use error::LexError;
pub use preprocess::PpValues;
pub use token::{Token, TokenKind};
pub use value::{Literal, ScriptString};

use literal::StringEnd;
use scaffold::Scaffold;

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

    /// Step over the whitespace and the newline markers.
    ///
    /// Returns whether there is a code unit left to read.
    pub fn skip_space(&mut self) -> bool {
        self.position = self.skip_space_at(self.position);
        self.current().is_some()
    }

    /// The position after the whitespace at a position.
    pub fn skip_space_at(&self, position: usize) -> usize {
        let mut position = position;
        while let Some(unit) = self.unit_at(position) {
            if unit == SKIP_CODE || is_space(unit) {
                position += 1;
            } else {
                break;
            }
        }
        position
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
pub(crate) fn is_space(unit: u16) -> bool {
    unit < 0x100 && matches!(unit, 0x20 | 0x09 | 0x0a | 0x0b | 0x0c | 0x0d)
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
pub(crate) fn is_name_unit(unit: u16) -> bool {
    is_digit(unit) || is_letter(unit) || unit == u16::from(b'_')
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
pub struct Lexer<'a> {
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
    conditional_values: &'a mut PpValues,
}

impl<'a> Lexer<'a> {
    /// An analyser for a script.
    ///
    /// The script is a sequence of code units; a code unit that is not a
    /// character of the text ends the script.
    ///
    /// The values the conditional-compilation statements read and write
    /// outlive the analyser: a value one script sets is visible in the next
    /// one.
    pub fn new(script: &[u16], conditional_values: &'a mut PpValues) -> Self {
        Self::with_text(script.to_vec(), false, false, conditional_values)
    }

    /// An analyser for an expression.
    ///
    /// The expression is read as a program whose last statement is
    /// terminated, and the analyser produces the token that returns the
    /// expression's value when `result_needed` is set.
    pub fn expression(
        script: &[u16],
        result_needed: bool,
        conditional_values: &'a mut PpValues,
    ) -> Self {
        Self::with_text(script.to_vec(), true, result_needed, conditional_values)
    }

    fn with_text(
        mut text: Vec<u16>,
        expression: bool,
        result_needed: bool,
        conditional_values: &'a mut PpValues,
    ) -> Self {
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
            conditional_values,
        }
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
            let scan = if self.embeddable.is_empty() {
                self.cursor.skip_space();
                self.scan_token()?
            } else {
                self.scan_embeddable()?
            };
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
            return Ok(Scan::Token(Token::with_value(
                TokenKind::Regexp,
                Literal::String(pattern),
            )));
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
                        return Ok(Scan::Token(Token::with_value(
                            TokenKind::ConstVal,
                            Literal::Octet(bytes),
                        )));
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
                    return Ok(Scan::Token(Token::with_value(
                        TokenKind::ConstVal,
                        Literal::String(scan.text),
                    )));
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
                    let name = self.scan_name();
                    let kind = if self.bare_word {
                        None
                    } else {
                        reserved_word(&name)
                    };
                    self.bare_word = false;
                    return Ok(Scan::Token(match kind {
                        Some(TokenKind::False) => {
                            Token::with_value(TokenKind::ConstVal, Literal::Integer(0))
                        }
                        Some(TokenKind::True) => {
                            Token::with_value(TokenKind::ConstVal, Literal::Integer(1))
                        }
                        Some(TokenKind::Null) => {
                            Token::with_value(TokenKind::ConstVal, Literal::Null)
                        }
                        Some(TokenKind::NaN) => {
                            Token::with_value(TokenKind::ConstVal, Literal::Real(f64::NAN))
                        }
                        Some(TokenKind::Infinity) => {
                            Token::with_value(TokenKind::ConstVal, Literal::Real(f64::INFINITY))
                        }
                        Some(kind) => Token::new(kind),
                        None => Token::with_value(
                            TokenKind::Symbol,
                            Literal::String(ScriptString::from_units(name)),
                        ),
                    }));
                }
            }
        }
    }

    /// The operator at the current position, if there is one.
    ///
    /// The table is matched over the characters at the cursor. The newline
    /// markers are stepped over while the text is matched, so an operator
    /// written across a newline pair is one operator.
    fn match_operator(&mut self) -> Option<TokenKind> {
        // the table holds ASCII characters only
        let mut text: [u8; 8] = [0; 8];
        let mut lengths: [u32; 8] = [0; 8];
        let mut count = 0;
        let mut position = self.cursor.position();
        let mut markers = 0;
        while count < text.len() {
            let Some(unit) = self.cursor.unit_at(position) else {
                break;
            };
            if unit == SKIP_CODE {
                markers += 1;
                position += 1;
                continue;
            }
            if unit >= 0x80 {
                break;
            }
            text[count] = unit as u8;
            lengths[count] = markers + 1;
            count += 1;
            markers = 0;
            position += 1;
        }
        if count == 0 {
            return None;
        }
        let mut scanner = Scaffold::lexer(&text[..count]);
        let Some(Ok(Scaffold::Operator(kind))) = scanner.next() else {
            return None;
        };
        let matched = scanner.span().end;
        // the matched characters, and the markers that follow the last of
        // them, are the code units the operator stands for
        let mut units: usize = lengths[..matched]
            .iter()
            .map(|&length| length as usize)
            .sum();
        if matched < count {
            units += (lengths[matched] - 1) as usize;
        } else {
            units += markers as usize;
        }
        self.cursor.move_to(self.cursor.position() + units);
        Some(kind)
    }

    /// The name at the current position.
    ///
    /// The name is the run of code units that can be part of a name. The
    /// newline markers inside it are not part of the name.
    fn scan_name(&mut self) -> Vec<u16> {
        let start = self.cursor.position();
        let mut end = start;
        while let Some(unit) = self.cursor.unit_at(end) {
            if is_name_unit(unit) || unit == SKIP_CODE {
                end += 1;
            } else {
                break;
            }
        }
        let mut name: Vec<u16> = Vec::with_capacity(end - start);
        for unit in &self.cursor.units()[start..end] {
            if *unit != SKIP_CODE {
                name.push(*unit);
            }
        }
        self.cursor.move_to(end);
        name
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
        match self.scan_conditional_statement()? {
            Statement::Continue => Ok(At::Continue),
            Statement::End => Ok(At::End),
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
                            self.pending.push_back(Token::with_value(
                                TokenKind::ConstVal,
                                Literal::String(scan.text),
                            ));
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
                            self.pending.push_back(Token::with_value(
                                TokenKind::ConstVal,
                                Literal::String(scan.text),
                            ));
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
            character: error::escape_character(character),
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

/// The reserved words and the token kinds they stand for.
const RESERVED_WORDS: &[(&str, TokenKind)] = &[
    ("break", TokenKind::Break),
    ("continue", TokenKind::Continue),
    ("const", TokenKind::Const),
    ("catch", TokenKind::Catch),
    ("class", TokenKind::Class),
    ("case", TokenKind::Case),
    ("debugger", TokenKind::Debugger),
    ("default", TokenKind::Default),
    ("delete", TokenKind::Delete),
    ("do", TokenKind::Do),
    ("extends", TokenKind::Extends),
    ("export", TokenKind::Export),
    ("enum", TokenKind::Enum),
    ("else", TokenKind::Else),
    ("function", TokenKind::Function),
    ("finally", TokenKind::Finally),
    ("false", TokenKind::False),
    ("for", TokenKind::For),
    ("global", TokenKind::Global),
    ("getter", TokenKind::Getter),
    ("goto", TokenKind::Goto),
    ("incontextof", TokenKind::InContextOf),
    ("invalidate", TokenKind::Invalidate),
    ("instanceof", TokenKind::InstanceOf),
    ("isvalid", TokenKind::IsValid),
    ("import", TokenKind::Import),
    ("int", TokenKind::Int),
    ("in", TokenKind::In),
    ("if", TokenKind::If),
    ("null", TokenKind::Null),
    ("new", TokenKind::New),
    ("octet", TokenKind::Octet),
    ("protected", TokenKind::Protected),
    ("property", TokenKind::Property),
    ("private", TokenKind::Private),
    ("public", TokenKind::Public),
    ("return", TokenKind::Return),
    ("real", TokenKind::Real),
    ("synchronized", TokenKind::Synchronized),
    ("switch", TokenKind::Switch),
    ("static", TokenKind::Static),
    ("setter", TokenKind::Setter),
    ("string", TokenKind::String),
    ("super", TokenKind::Super),
    ("typeof", TokenKind::TypeOf),
    ("throw", TokenKind::Throw),
    ("this", TokenKind::This),
    ("true", TokenKind::True),
    ("try", TokenKind::Try),
    ("void", TokenKind::Void),
    ("var", TokenKind::Var),
    ("while", TokenKind::While),
    ("NaN", TokenKind::NaN),
    ("Infinity", TokenKind::Infinity),
    ("with", TokenKind::With),
];

/// The token kind a reserved word stands for.
fn reserved_word(name: &[u16]) -> Option<TokenKind> {
    RESERVED_WORDS
        .iter()
        .find(|(word, _)| {
            word.len() == name.len()
                && word
                    .bytes()
                    .zip(name)
                    .all(|(expected, unit)| u16::from(expected) == *unit)
        })
        .map(|(_, kind)| *kind)
}
