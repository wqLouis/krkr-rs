//! The parser: the token stream in, the expression tree out.
//!
//! The parser reads the tokens the analyser produces and builds the
//! expression tree the grammar's actions build. The tree is the
//! generator's node type (`codegen::node::Node`): the parser makes the
//! nodes, and the generator walks them.
//!
//! The parser follows the grammar's productions: the same expressions,
//! the same statements, the same commands to the generator.
//!
//! The commands the grammar drives are the generator's, and the parser
//! reaches them through the [`Commands`] trait: the trait is the command
//! surface the grammar needs, and the generator's own wrapper is one
//! implementation of it.
//!
//! # The places in the script
//!
//! Every node carries the place in the script it was made at. The place is
//! the analyser's own place, counted in code units from the start of the
//! script, and it is the place the generator gives a node.
//!
//! The place therefore depends on when the parser reads the token that
//! follows the expression: the parser reads the lookahead before it
//! reduces a rule whose last symbol is an expression, and it reduces
//! without a lookahead when the rule's last symbol is a token.
//! The parser follows the same discipline: a node is made when the rule
//! that makes it is complete.
//!
//! The instructions that have no place of their own take the place the
//! analyser is at, and the parser hands that place to the generator
//! every time the analyser reads. The generator holds it, so it writes
//! the place the analyser is at and not the place of the last node.

#![deny(unsafe_code)]

mod error;
mod nodes;

pub use error::ParseError;
pub use nodes::Nodes;

use std::collections::VecDeque;
use std::rc::Rc;

use crate::vm::codegen::context::ContextType;
use crate::vm::codegen::node::Node;
use crate::vm::codegen::token;
use crate::vm::codegen::value::{ObjectId, Value};
use crate::vm::lexer::{Lexer, Token, TokenKind, ValueRef};

/// The commands the grammar drives.
///
/// The commands are the generator's: the parser reads the script and
/// hands the generator the expressions and the statements it finds. The
/// names are the generator's names.
pub trait Commands {
    /// The code of an expression that stands on its own.
    fn create_expr_code(&mut self, node: &Rc<Node>)
    -> Result<(), crate::vm::codegen::CodegenError>;

    /// A local variable of the context.
    fn add_local_variable(&mut self, name: &str, init: i32);

    /// A local variable of the context, with its first value.
    fn init_local_variable(
        &mut self,
        name: &str,
        node: &Rc<Node>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// A declared argument of a function.
    fn add_function_decl_arg(
        &mut self,
        name: &str,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The collapsed argument of a function.
    fn add_function_decl_arg_collapse(&mut self, name: Option<&str>);

    /// The argument of a property setter.
    fn set_property_decl_arg(&mut self, name: &str);

    /// Opens a code context.
    fn push_context(&mut self, name: Option<&[u16]>, kind: ContextType) -> usize;

    /// Closes the code context the code is generated for.
    fn pop_context(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens an `if`.
    fn enter_if_code(&mut self);

    /// The code of the condition of an `if`.
    fn create_if_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Closes an `if`.
    fn exit_if_code(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens an `else`.
    fn enter_else_code(&mut self);

    /// Closes an `else`.
    fn exit_else_code(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens a `while` loop.
    fn enter_while_code(&mut self, do_while: bool);

    /// The code of the condition of a `while` loop.
    fn create_while_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        do_while: bool,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Closes a `while` loop.
    fn exit_while_code(&mut self, do_while: bool) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens a `for` loop.
    fn enter_for_code(&mut self);

    /// The code of the condition of a `for` loop.
    fn create_for_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The code of the third clause of a `for` loop.
    fn set_for_third_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Closes a `for` loop.
    fn exit_for_code(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens a `switch`.
    fn enter_switch_code(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Closes a `switch`.
    fn exit_switch_code(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The code of a `case` or of a `default`.
    fn process_case_code(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens a `with`.
    fn enter_with_code(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Closes a `with`.
    fn exit_with_code(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens a `try`.
    fn enter_try_code(&mut self);

    /// Opens the `catch` of a `try`.
    fn enter_catch_code(
        &mut self,
        name: Option<&str>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Closes a `try`.
    fn exit_try_code(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The code of a `throw`.
    fn process_throw_code(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The code of a `break`.
    fn do_break(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The code of a `continue`.
    fn do_continue(&mut self) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The code of a `debugger`.
    fn do_debugger(&mut self);

    /// The code of a `return`.
    fn return_from_func(
        &mut self,
        node: Option<&Rc<Node>>,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// The code of the super class of a class.
    fn create_extends_expr_code(
        &mut self,
        node: Option<&Rc<Node>>,
        hold: bool,
    ) -> Result<(), crate::vm::codegen::CodegenError>;

    /// Opens a block.
    fn enter_block(&mut self);

    /// Closes a block.
    fn exit_block(&mut self);

    /// The place in the script the analyser is at.
    ///
    /// The generator writes this place into the instructions that have
    /// no place of their own. The parser tells the generator where the
    /// analyser is every time the analyser reads, so the place the
    /// generator holds is the place the analyser is at.
    fn at_lex_position(&mut self, position: i32);
}

/// One argument of a call.
///
/// The argument is made a node only when the argument is an expression:
/// the empty argument and the argument that expands the arguments are
/// made when the list is reduced.
#[derive(Clone, Debug)]
enum CallArg {
    /// The argument is empty.
    Empty,
    /// The argument expands the arguments of the call, and holds nothing.
    ExpandStar,
    /// The argument expands the arguments of the call, and holds the
    /// expression that ends with the star.
    Expand(Rc<Node>),
    /// The argument is an expression.
    Node(Rc<Node>),
}

/// What the parser produced.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// The nodes the parser made, in the order it made them.
    pub nodes: Vec<Rc<Node>>,
    /// The number of failures the parser reported.
    pub error_count: usize,
    /// The first failure the parser reported.
    pub first_error: Option<ParseError>,
}

/// The parser: the tokens in, the tree out.
///
/// The parser holds the analyser it reads, the commands it hands the
/// generator, and the nodes it made.
pub struct Parser<'a, C: Commands> {
    /// The analyser the parser reads.
    lexer: &'a mut Lexer,
    /// The commands the parser hands the generator.
    commands: C,
    /// The tokens that were read but not consumed.
    ///
    /// The parser reads one token ahead. It reads a second one only to
    /// tell a cast from a parenthesized expression, and the token it
    /// reads then is the first token of an expression — so a regular
    /// expression that starts there is read as one.
    lookahead: VecDeque<Token>,
    /// The analyser reached the end of the script.
    at_end: bool,
    /// The nodes the parser made.
    nodes: Nodes,
    /// The nodes the inline array and the inline dictionary are built in.
    current_nodes: Vec<usize>,
    /// The number of failures the parser reported.
    error_count: usize,
    /// The first failure the parser reported.
    first_error: Option<ParseError>,
    /// The parser stopped: too many failures.
    stopped: bool,
}

impl<'a, C: Commands> Parser<'a, C> {
    /// A parser over the given analyser.
    pub fn new(lexer: &'a mut Lexer, commands: C) -> Self {
        Self {
            lexer,
            commands,
            lookahead: VecDeque::new(),
            at_end: false,
            nodes: Nodes::default(),
            current_nodes: Vec::new(),
            error_count: 0,
            first_error: None,
            stopped: false,
        }
    }

    /// Parses the script the analyser reads.
    ///
    /// The parser recovers from a failure the way the grammar's
    /// `def_list error ";"` rule does: the tokens are discarded up to the
    /// `;`, and the parse goes on. The failures are counted, and the parse
    /// stops when there are more than twenty of them.
    pub fn parse(mut self) -> Parsed {
        if let Err(error) = self.program() {
            // the abort is not a failure the count sees: the grammar
            // stops the parse with `YYABORT`, which the count does not
            // see
            if !matches!(error, ParseError::Aborted) {
                self.report(error);
            }
        }
        Parsed {
            nodes: self.nodes.all().to_vec(),
            error_count: self.error_count,
            first_error: self.first_error.clone(),
        }
    }

    /// The commands the parser hands the generator.
    pub fn commands(&self) -> &C {
        &self.commands
    }

    // -- the token stream --------------------------------------------------

    /// Reads tokens until the buffer holds the given number of them.
    fn fill(&mut self, count: usize) {
        while !self.at_end && self.lookahead.len() < count {
            match self.lexer.next_token() {
                Ok(Some(token)) => self.lookahead.push_back(token),
                Ok(None) => self.at_end = true,
                Err(error) => {
                    self.at_end = true;
                    self.report(ParseError::Lexical {
                        message: error.to_string(),
                        position: error.position() as i32,
                    });
                }
            }
            // the analyser has read: the generator's place follows it
            let position = self.position();
            self.commands.at_lex_position(position);
        }
    }

    /// The token the parser looks at.
    fn peek(&mut self) -> Option<Token> {
        self.fill(1);
        self.lookahead.front().copied()
    }

    /// The kind of the token the parser looks at.
    fn peek_kind(&mut self) -> Option<TokenKind> {
        self.peek().map(|token| token.kind)
    }

    /// The token after the one the parser looks at.
    fn peek_second(&mut self) -> Option<Token> {
        self.fill(2);
        self.lookahead.get(1).copied()
    }

    /// The token after the one the parser looks at, and the one after it.
    fn peek_third(&mut self) -> Option<Token> {
        self.fill(3);
        self.lookahead.get(2).copied()
    }

    /// Reads the token at the given place again, as a regular expression.
    ///
    /// The token is a slash that starts a regular expression: the
    /// analyser must read it as one, and it reads it again from the slash
    /// (`crates/tjs/rust/script/src/lexer/mod.rs:520-528`).
    fn read_regexp_again(&mut self, index: usize) {
        self.lexer.expect_regexp();
        self.lookahead.remove(index);
        self.fill(index + 1);
    }

    /// Consumes the token the parser looks at.
    fn advance(&mut self) -> Option<Token> {
        self.fill(1);
        self.lookahead.pop_front()
    }

    /// The place in the script the analyser is at.
    fn position(&self) -> i32 {
        self.lexer.current_position() as i32
    }

    /// The name of a token, the way a message writes it.
    fn token_name(kind: TokenKind) -> &'static str {
        match kind {
            TokenKind::Comma => "','",
            TokenKind::Equal => "'='",
            TokenKind::AmpersandAssign => "'&='",
            TokenKind::VerticalBarAssign => "'|='",
            TokenKind::ChevronAssign => "'^='",
            TokenKind::MinusAssign => "'-='",
            TokenKind::PlusAssign => "'+='",
            TokenKind::PercentAssign => "'%='",
            TokenKind::SlashAssign => "'/='",
            TokenKind::BackslashAssign => "'\\='",
            TokenKind::AsteriskAssign => "'*='",
            TokenKind::LogicalOrAssign => "'||='",
            TokenKind::LogicalAndAssign => "'&&='",
            TokenKind::RightBitShiftAssign => "'>>>='",
            TokenKind::LeftBitShiftAssign => "'<<='",
            TokenKind::RightArithmeticShiftAssign => "'>>='",
            TokenKind::Question => "'?'",
            TokenKind::LogicalOr => "'||'",
            TokenKind::LogicalAnd => "'&&'",
            TokenKind::VerticalBar => "'|'",
            TokenKind::Chevron => "'^'",
            TokenKind::Ampersand => "'&'",
            TokenKind::NotEqual => "'!='",
            TokenKind::EqualEqual => "'=='",
            TokenKind::DiscriminantNotEqual => "'!=='",
            TokenKind::DiscriminantEqual => "'==='",
            TokenKind::Swap => "'<->'",
            TokenKind::LessThan => "'<'",
            TokenKind::GreaterThan => "'>'",
            TokenKind::LessOrEqual => "'<='",
            TokenKind::GreaterOrEqual => "'>='",
            TokenKind::RightArithmeticShift => "'>>'",
            TokenKind::LeftBitShift => "'<<'",
            TokenKind::RightBitShift => "'>>>'",
            TokenKind::Percent => "'%'",
            TokenKind::Slash => "'/'",
            TokenKind::Backslash => "'\\'",
            TokenKind::Asterisk => "'*'",
            TokenKind::Exclamation => "'!'",
            TokenKind::Tilde => "'~'",
            TokenKind::Decrement => "'--'",
            TokenKind::Increment => "'++'",
            TokenKind::New => "'new'",
            TokenKind::Delete => "'delete'",
            TokenKind::TypeOf => "'typeof'",
            TokenKind::Plus => "'+'",
            TokenKind::Minus => "'-'",
            TokenKind::Sharp => "'#'",
            TokenKind::Dollar => "'$'",
            TokenKind::IsValid => "'isvalid'",
            TokenKind::Invalidate => "'invalidate'",
            TokenKind::InstanceOf => "'instanceof'",
            TokenKind::LeftParenthesis => "'('",
            TokenKind::Dot => "'.'",
            TokenKind::LeftBracket => "'['",
            TokenKind::This => "'this'",
            TokenKind::Super => "'super'",
            TokenKind::Global => "'global'",
            TokenKind::RightBracket => "']'",
            TokenKind::Class => "'class'",
            TokenKind::RightParenthesis => "')'",
            TokenKind::Colon => "':'",
            TokenKind::Semicolon => "';'",
            TokenKind::LeftBrace => "'{'",
            TokenKind::RightBrace => "'}'",
            TokenKind::Continue => "'continue'",
            TokenKind::Function => "'function'",
            TokenKind::Debugger => "'debugger'",
            TokenKind::Default => "'default'",
            TokenKind::Case => "'case'",
            TokenKind::Extends => "'extends'",
            TokenKind::Finally => "'finally'",
            TokenKind::Property => "'property'",
            TokenKind::Private => "'private'",
            TokenKind::Public => "'public'",
            TokenKind::Protected => "'protected'",
            TokenKind::Static => "'static'",
            TokenKind::Return => "'return'",
            TokenKind::Break => "'break'",
            TokenKind::Export => "'export'",
            TokenKind::Import => "'import'",
            TokenKind::Switch => "'switch'",
            TokenKind::In => "'in'",
            TokenKind::InContextOf => "'incontextof'",
            TokenKind::For => "'for'",
            TokenKind::While => "'while'",
            TokenKind::Do => "'do'",
            TokenKind::If => "'if'",
            TokenKind::Var => "'var'",
            TokenKind::Const => "'const'",
            TokenKind::Enum => "'enum'",
            TokenKind::Goto => "'goto'",
            TokenKind::Throw => "'throw'",
            TokenKind::Try => "'try'",
            TokenKind::Setter => "'setter'",
            TokenKind::Getter => "'getter'",
            TokenKind::Else => "'else'",
            TokenKind::Catch => "'catch'",
            TokenKind::Omit => "'...'",
            TokenKind::Synchronized => "'synchronized'",
            TokenKind::With => "'with'",
            TokenKind::Int => "'int'",
            TokenKind::Real => "'real'",
            TokenKind::String => "'string'",
            TokenKind::Octet => "'octet'",
            TokenKind::False => "'false'",
            TokenKind::Null => "'null'",
            TokenKind::True => "'true'",
            TokenKind::Void => "'void'",
            TokenKind::NaN => "'NaN'",
            TokenKind::Infinity => "'Infinity'",
            TokenKind::ConstVal => "a constant",
            TokenKind::Symbol => "a name",
            TokenKind::Regexp => "a regular expression",
        }
    }

    /// The failure the parser reports when the token does not fit.
    ///
    /// The grammar is built with bison's simple error messages, so the
    /// message the parser makes is "syntax error"; `_yyerror` turns that
    /// into the message the script block raises.
    ///
    /// The token the parser expected is kept beside the message, not in
    /// it: the message does not name it.
    fn syntax_error(&mut self, expected: &str) -> ParseError {
        let expected = match self.peek_kind() {
            Some(kind) => Some(format!("{} {}", Self::token_name(kind), expected)),
            None => Some(format!("the end of the script, expecting {expected}")),
        };
        ParseError::Syntax {
            message: "Syntax error (syntax error)".to_string(),
            position: self.position(),
            expected,
        }
    }

    /// Reports a failure.
    fn report(&mut self, error: ParseError) {
        if self.error_count == 0 {
            self.first_error = Some(error);
        }
        self.error_count += 1;
    }

    /// Consumes the token the parser looks at, when it is the given kind.
    fn expect(&mut self, kind: TokenKind) -> Result<Token, ParseError> {
        match self.peek_kind() {
            Some(found) if found == kind => Ok(self.advance().unwrap_or(Token::new(kind))),
            _ => Err(self.syntax_error(Self::token_name(kind))),
        }
    }

    /// Consumes the token the parser looks at, when it is a name.
    fn expect_symbol(&mut self) -> Result<String, ParseError> {
        let token = match self.peek_kind() {
            Some(TokenKind::Symbol) => self.advance().unwrap_or(Token::new(TokenKind::Symbol)),
            _ => return Err(self.syntax_error("a name")),
        };
        Ok(self.name_of(&token))
    }

    /// The name a token carries.
    fn name_of(&self, token: &Token) -> String {
        match self.lexer.value(token) {
            Some(ValueRef::String(units)) => String::from_utf16_lossy(units),
            _ => String::new(),
        }
    }

    // -- the grammar --------------------------------------------------------

    /// The program.
    fn program(&mut self) -> Result<(), ParseError> {
        self.global_list()?;
        // the program is the global list and the end of the script: a
        // token that follows it is a failure
        if self.peek_kind().is_some() {
            return Err(self.syntax_error("the end of the script"));
        }
        Ok(())
    }

    /// The global definitions.
    fn global_list(&mut self) -> Result<(), ParseError> {
        let name: Vec<u16> = "global".encode_utf16().collect();
        self.commands
            .push_context(Some(&name), ContextType::TopLevel);
        let result = self.def_list();
        self.commands.pop_context()?;
        result
    }

    /// The definition list.
    fn def_list(&mut self) -> Result<(), ParseError> {
        loop {
            if self.stopped {
                return Ok(());
            }
            match self.peek_kind() {
                None | Some(TokenKind::RightBrace) => return Ok(()),
                _ => {}
            }
            if let Err(error) = self.block_or_statement() {
                self.report(error);
                self.recover()?;
            }
        }
    }

    /// Discards the tokens up to the `;`.
    ///
    /// This is the recovery the grammar's `def_list error ";"` rule does.
    fn recover(&mut self) -> Result<(), ParseError> {
        if self.error_count > 20 {
            self.stopped = true;
            return Err(ParseError::Aborted);
        }
        loop {
            match self.peek_kind() {
                Some(TokenKind::Semicolon) => {
                    self.advance();
                    return Ok(());
                }
                None => {
                    self.stopped = true;
                    return Err(ParseError::Aborted);
                }
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// A block or a single statement.
    fn block_or_statement(&mut self) -> Result<(), ParseError> {
        if self.peek_kind() == Some(TokenKind::LeftBrace) {
            self.block()
        } else {
            self.statement()
        }
    }

    /// A block.
    fn block(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::LeftBrace)?;
        self.commands.enter_block();
        let result = self.def_list();
        match result {
            Ok(()) => {
                self.expect(TokenKind::RightBrace)?;
                self.commands.exit_block();
                Ok(())
            }
            Err(error) => {
                self.commands.exit_block();
                Err(error)
            }
        }
    }

    /// A statement.
    fn statement(&mut self) -> Result<(), ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Semicolon) => {
                self.advance();
                Ok(())
            }
            Some(TokenKind::Break) => {
                self.advance();
                self.expect(TokenKind::Semicolon)?;
                Ok(self.commands.do_break()?)
            }
            Some(TokenKind::Continue) => {
                self.advance();
                self.expect(TokenKind::Semicolon)?;
                Ok(self.commands.do_continue()?)
            }
            Some(TokenKind::Debugger) => {
                self.advance();
                self.expect(TokenKind::Semicolon)?;
                self.commands.do_debugger();
                Ok(())
            }
            Some(TokenKind::Var) | Some(TokenKind::Const) => self.variable_def(),
            Some(TokenKind::Function) => {
                // a function definition and a function expression are told
                // apart by the name that follows the keyword
                if self.function_defines() {
                    self.func_def()
                } else {
                    self.expression_statement()
                }
            }
            Some(TokenKind::Class) => self.class_def(),
            Some(TokenKind::Property) => self.property_def(),
            Some(TokenKind::Return) => self.return_statement(),
            Some(TokenKind::Switch) => self.switch_statement(),
            Some(TokenKind::With) => self.with_statement(),
            Some(TokenKind::Case) | Some(TokenKind::Default) => self.case_statement(),
            Some(TokenKind::Try) => self.try_statement(),
            Some(TokenKind::Throw) => self.throw_statement(),
            Some(TokenKind::If) => self.if_statement(),
            Some(TokenKind::While) => self.while_statement(),
            Some(TokenKind::Do) => self.do_while_statement(),
            Some(TokenKind::For) => self.for_statement(),
            _ => self.expression_statement(),
        }
    }

    /// Whether the `function` the parser looks at defines a function.
    ///
    /// A definition has a name; an expression does not.
    fn function_defines(&mut self) -> bool {
        self.fill(2);
        self.lookahead
            .get(1)
            .is_some_and(|token| token.kind == TokenKind::Symbol)
    }

    /// An expression that stands on its own.
    fn expression_statement(&mut self) -> Result<(), ParseError> {
        let node = self.expr()?;
        self.expect(TokenKind::Semicolon)?;
        Ok(self.commands.create_expr_code(&node)?)
    }

    /// An `if` statement, with the `else` that belongs to it.
    fn if_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.commands.enter_if_code();
        self.expect(TokenKind::LeftParenthesis)?;
        let condition = self.expr()?;
        self.commands.create_if_expr_code(Some(&condition))?;
        self.expect(TokenKind::RightParenthesis)?;
        self.block_or_statement()?;
        self.commands.exit_if_code()?;
        if self.peek_kind() == Some(TokenKind::Else) {
            self.advance();
            self.commands.enter_else_code();
            self.block_or_statement()?;
            self.commands.exit_else_code()?;
        }
        Ok(())
    }

    /// A `while` loop.
    fn while_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.commands.enter_while_code(false);
        self.expect(TokenKind::LeftParenthesis)?;
        let condition = self.expr()?;
        self.commands
            .create_while_expr_code(Some(&condition), false)?;
        self.expect(TokenKind::RightParenthesis)?;
        self.block_or_statement()?;
        Ok(self.commands.exit_while_code(false)?)
    }

    /// A `do` loop.
    fn do_while_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.commands.enter_while_code(true);
        self.block_or_statement()?;
        self.expect(TokenKind::While)?;
        self.expect(TokenKind::LeftParenthesis)?;
        let condition = self.expr()?;
        self.commands
            .create_while_expr_code(Some(&condition), true)?;
        self.expect(TokenKind::RightParenthesis)?;
        self.expect(TokenKind::Semicolon)?;
        Ok(self.commands.exit_while_code(true)?)
    }

    /// A `for` loop.
    fn for_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.expect(TokenKind::LeftParenthesis)?;

        // the first clause
        match self.peek_kind() {
            Some(TokenKind::Var) | Some(TokenKind::Const) => {
                self.commands.enter_for_code();
                self.variable_def_inner()?;
            }
            Some(TokenKind::Semicolon) | None => {
                self.commands.enter_for_code();
            }
            _ => {
                let node = self.expr()?;
                self.commands.enter_for_code();
                self.commands.create_expr_code(&node)?;
            }
        }
        self.expect(TokenKind::Semicolon)?;

        // the second clause
        match self.peek_kind() {
            Some(TokenKind::Semicolon) | None => {
                self.commands.create_for_expr_code(None)?;
            }
            _ => {
                let node = self.expr()?;
                self.commands.create_for_expr_code(Some(&node))?;
            }
        }
        self.expect(TokenKind::Semicolon)?;

        // the third clause
        match self.peek_kind() {
            Some(TokenKind::RightParenthesis) | None => {
                self.commands.set_for_third_expr_code(None)?;
            }
            _ => {
                let node = self.expr()?;
                self.commands.set_for_third_expr_code(Some(&node))?;
            }
        }
        self.expect(TokenKind::RightParenthesis)?;

        self.block_or_statement()?;
        Ok(self.commands.exit_for_code()?)
    }

    /// A variable definition.
    fn variable_def(&mut self) -> Result<(), ParseError> {
        self.variable_def_inner()?;
        self.expect(TokenKind::Semicolon)?;
        Ok(())
    }

    /// The inside of a variable definition.
    fn variable_def_inner(&mut self) -> Result<(), ParseError> {
        self.advance();
        loop {
            self.variable_id()?;
            if self.peek_kind() == Some(TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(())
    }

    /// A variable and its first value.
    fn variable_id(&mut self) -> Result<(), ParseError> {
        let name = self.expect_symbol()?;
        self.variable_type()?;
        if self.peek_kind() == Some(TokenKind::Equal) {
            self.advance();
            let node = self.expr_no_comma()?;
            Ok(self.commands.init_local_variable(&name, &node)?)
        } else {
            self.commands.add_local_variable(&name, 0);
            Ok(())
        }
    }

    /// The type of a variable. The types are ignored.
    fn variable_type(&mut self) -> Result<(), ParseError> {
        if self.peek_kind() != Some(TokenKind::Colon) {
            return Ok(());
        }
        self.advance();
        match self.peek_kind() {
            Some(TokenKind::Symbol)
            | Some(TokenKind::Void)
            | Some(TokenKind::Int)
            | Some(TokenKind::Real)
            | Some(TokenKind::String)
            | Some(TokenKind::Octet) => {
                self.advance();
                Ok(())
            }
            _ => Err(self.syntax_error("a type")),
        }
    }

    /// A function definition.
    fn func_def(&mut self) -> Result<(), ParseError> {
        self.advance();
        let name = self.expect_symbol()?;
        let units: Vec<u16> = name.encode_utf16().collect();
        self.commands
            .push_context(Some(&units), ContextType::Function);
        self.commands.enter_block();
        let result = (|| {
            self.func_decl_arg_opt()?;
            self.variable_type()?;
            self.block()
        })();
        self.commands.exit_block();
        self.commands.pop_context()?;
        result
    }

    /// The arguments of a function.
    fn func_decl_arg_opt(&mut self) -> Result<(), ParseError> {
        if self.peek_kind() != Some(TokenKind::LeftParenthesis) {
            return Ok(());
        }
        self.advance();
        loop {
            match self.peek_kind() {
                Some(TokenKind::RightParenthesis) => break,
                Some(TokenKind::Asterisk) => {
                    self.advance();
                    self.commands.add_function_decl_arg_collapse(None);
                    break;
                }
                Some(TokenKind::Symbol) => {
                    let name = self.expect_symbol()?;
                    self.variable_type()?;
                    if self.peek_kind() == Some(TokenKind::Asterisk) {
                        self.advance();
                        self.commands.add_function_decl_arg_collapse(Some(&name));
                        break;
                    }
                    if self.peek_kind() == Some(TokenKind::Equal) {
                        self.advance();
                        let node = self.expr_no_comma()?;
                        self.commands.add_function_decl_arg(&name, Some(&node))?;
                    } else {
                        self.commands.add_function_decl_arg(&name, None)?;
                    }
                }
                _ => return Err(self.syntax_error("an argument")),
            }
            if self.peek_kind() == Some(TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RightParenthesis)?;
        Ok(())
    }

    /// A property definition.
    fn property_def(&mut self) -> Result<(), ParseError> {
        self.advance();
        let name = self.expect_symbol()?;
        self.expect(TokenKind::LeftBrace)?;
        let units: Vec<u16> = name.encode_utf16().collect();
        self.commands
            .push_context(Some(&units), ContextType::Property);
        let result = self.property_handler_def_list();
        self.commands.pop_context()?;
        result?;
        self.expect(TokenKind::RightBrace)?;
        Ok(())
    }

    /// The handlers of a property.
    fn property_handler_def_list(&mut self) -> Result<(), ParseError> {
        if self.peek_kind() == Some(TokenKind::Setter) {
            self.property_handler_setter()?;
            if self.peek_kind() == Some(TokenKind::Getter) {
                self.property_handler_getter()?;
            }
        } else if self.peek_kind() == Some(TokenKind::Getter) {
            self.property_handler_getter()?;
            if self.peek_kind() == Some(TokenKind::Setter) {
                self.property_handler_setter()?;
            }
        }
        Ok(())
    }

    /// The setter of a property.
    fn property_handler_setter(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.expect(TokenKind::LeftParenthesis)?;
        let name = self.expect_symbol()?;
        self.variable_type()?;
        self.expect(TokenKind::RightParenthesis)?;
        let units: Vec<u16> = "(setter)".encode_utf16().collect();
        self.commands
            .push_context(Some(&units), ContextType::PropertySetter);
        self.commands.enter_block();
        self.commands.set_property_decl_arg(&name);
        let result = self.block();
        self.commands.exit_block();
        self.commands.pop_context()?;
        result
    }

    /// The getter of a property.
    fn property_handler_getter(&mut self) -> Result<(), ParseError> {
        self.advance();
        if self.peek_kind() == Some(TokenKind::LeftParenthesis) {
            self.advance();
            self.expect(TokenKind::RightParenthesis)?;
        }
        self.variable_type()?;
        let units: Vec<u16> = "(getter)".encode_utf16().collect();
        self.commands
            .push_context(Some(&units), ContextType::PropertyGetter);
        self.commands.enter_block();
        let result = self.block();
        self.commands.exit_block();
        self.commands.pop_context()?;
        result
    }

    /// A class definition.
    fn class_def(&mut self) -> Result<(), ParseError> {
        self.advance();
        let name = self.expect_symbol()?;
        let units: Vec<u16> = name.encode_utf16().collect();
        self.commands.push_context(Some(&units), ContextType::Class);
        let result = (|| {
            self.class_extender()?;
            self.block()
        })();
        self.commands.pop_context()?;
        result
    }

    /// The super class of a class.
    fn class_extender(&mut self) -> Result<(), ParseError> {
        if self.peek_kind() != Some(TokenKind::Extends) {
            return Ok(());
        }
        self.advance();
        let node = self.expr_no_comma()?;
        if self.peek_kind() == Some(TokenKind::Comma) {
            self.advance();
            self.commands.create_extends_expr_code(Some(&node), false)?;
            self.extends_list()
        } else {
            Ok(self.commands.create_extends_expr_code(Some(&node), true)?)
        }
    }

    /// The list of the super classes of a class.
    fn extends_list(&mut self) -> Result<(), ParseError> {
        loop {
            let node = self.expr_no_comma()?;
            self.commands.create_extends_expr_code(Some(&node), false)?;
            if self.peek_kind() == Some(TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(())
    }

    /// A `return` statement.
    fn return_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        if self.peek_kind() == Some(TokenKind::Semicolon) {
            self.advance();
            Ok(self.commands.return_from_func(None)?)
        } else {
            let node = self.expr()?;
            self.expect(TokenKind::Semicolon)?;
            Ok(self.commands.return_from_func(Some(&node))?)
        }
    }

    /// A `switch` statement.
    fn switch_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.expect(TokenKind::LeftParenthesis)?;
        let node = self.expr()?;
        self.expect(TokenKind::RightParenthesis)?;
        self.commands.enter_switch_code(Some(&node))?;
        let result = self.block();
        self.commands.exit_switch_code()?;
        result
    }

    /// A `with` statement.
    fn with_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.expect(TokenKind::LeftParenthesis)?;
        let node = self.expr()?;
        self.expect(TokenKind::RightParenthesis)?;
        self.commands.enter_with_code(Some(&node))?;
        let result = self.block_or_statement();
        self.commands.exit_with_code()?;
        result
    }

    /// A `case` or a `default`.
    fn case_statement(&mut self) -> Result<(), ParseError> {
        if self.peek_kind() == Some(TokenKind::Case) {
            self.advance();
            let node = self.expr()?;
            self.expect(TokenKind::Colon)?;
            Ok(self.commands.process_case_code(Some(&node))?)
        } else {
            self.advance();
            self.expect(TokenKind::Colon)?;
            Ok(self.commands.process_case_code(None)?)
        }
    }

    /// A `try` statement.
    fn try_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        self.commands.enter_try_code();
        self.block_or_statement()?;
        self.catch_clause()?;
        self.block_or_statement()?;
        Ok(self.commands.exit_try_code()?)
    }

    /// The `catch` of a `try`.
    fn catch_clause(&mut self) -> Result<(), ParseError> {
        self.expect(TokenKind::Catch)?;
        if self.peek_kind() != Some(TokenKind::LeftParenthesis) {
            return Ok(self.commands.enter_catch_code(None)?);
        }
        self.advance();
        if self.peek_kind() == Some(TokenKind::Symbol) {
            let name = self.expect_symbol()?;
            self.expect(TokenKind::RightParenthesis)?;
            Ok(self.commands.enter_catch_code(Some(&name))?)
        } else {
            self.expect(TokenKind::RightParenthesis)?;
            Ok(self.commands.enter_catch_code(None)?)
        }
    }

    /// A `throw` statement.
    fn throw_statement(&mut self) -> Result<(), ParseError> {
        self.advance();
        let node = self.expr()?;
        self.expect(TokenKind::Semicolon)?;
        Ok(self.commands.process_throw_code(Some(&node))?)
    }

    // -- the expressions ----------------------------------------------------

    /// An expression that holds no comma.
    fn expr_no_comma(&mut self) -> Result<Rc<Node>, ParseError> {
        self.assign_expr()
    }

    /// An expression.
    fn expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let node = self.comma_expr()?;
        if self.peek_kind() == Some(TokenKind::If) {
            self.advance();
            let condition = self.expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(token::T_IF, position, Some(&node), Some(&condition));
            return Ok(self.nodes.node(index));
        }
        Ok(node)
    }

    /// A comma expression.
    fn comma_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.assign_expr()?;
        while self.peek_kind() == Some(TokenKind::Comma) {
            self.advance();
            let right = self.assign_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(token::T_COMMA, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// An assignment.
    fn assign_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let node = self.cond_expr()?;
        let opcode = match self.peek_kind() {
            Some(TokenKind::Swap) => token::T_SWAP,
            Some(TokenKind::Equal) => token::T_EQUAL,
            Some(TokenKind::AmpersandAssign) => token::T_AMPERSANDEQUAL,
            Some(TokenKind::VerticalBarAssign) => token::T_VERTLINEEQUAL,
            Some(TokenKind::ChevronAssign) => token::T_CHEVRONEQUAL,
            Some(TokenKind::MinusAssign) => token::T_MINUSEQUAL,
            Some(TokenKind::PlusAssign) => token::T_PLUSEQUAL,
            Some(TokenKind::PercentAssign) => token::T_PERCENTEQUAL,
            Some(TokenKind::SlashAssign) => token::T_SLASHEQUAL,
            Some(TokenKind::BackslashAssign) => token::T_BACKSLASHEQUAL,
            Some(TokenKind::AsteriskAssign) => token::T_ASTERISKEQUAL,
            Some(TokenKind::LogicalOrAssign) => token::T_LOGICALOREQUAL,
            Some(TokenKind::LogicalAndAssign) => token::T_LOGICALANDEQUAL,
            Some(TokenKind::RightArithmeticShiftAssign) => token::T_RARITHSHIFTEQUAL,
            Some(TokenKind::LeftBitShiftAssign) => token::T_LARITHSHIFTEQUAL,
            Some(TokenKind::RightBitShiftAssign) => token::T_RBITSHIFTEQUAL,
            _ => return Ok(node),
        };
        self.advance();
        let right = self.assign_expr()?;
        let position = self.position();
        let index = self
            .nodes
            .make_np2(opcode, position, Some(&node), Some(&right));
        Ok(self.nodes.node(index))
    }

    /// A conditional expression.
    fn cond_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let node = self.logical_or_expr()?;
        if self.peek_kind() != Some(TokenKind::Question) {
            return Ok(node);
        }
        self.advance();
        let then_branch = self.cond_expr()?;
        self.expect(TokenKind::Colon)?;
        let else_branch = self.cond_expr()?;
        let position = self.position();
        let index = self.nodes.make_np3(
            token::T_QUESTION,
            position,
            Some(&node),
            Some(&then_branch),
            Some(&else_branch),
        );
        Ok(self.nodes.node(index))
    }

    /// A logical or.
    fn logical_or_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.logical_and_expr()?;
        while self.peek_kind() == Some(TokenKind::LogicalOr) {
            self.advance();
            let right = self.logical_and_expr()?;
            let position = self.position();
            let index =
                self.nodes
                    .make_np2(token::T_LOGICALOR, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// A logical and.
    fn logical_and_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.inclusive_or_expr()?;
        while self.peek_kind() == Some(TokenKind::LogicalAnd) {
            self.advance();
            let right = self.inclusive_or_expr()?;
            let position = self.position();
            let index =
                self.nodes
                    .make_np2(token::T_LOGICALAND, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// An inclusive or.
    fn inclusive_or_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.exclusive_or_expr()?;
        while self.peek_kind() == Some(TokenKind::VerticalBar) {
            self.advance();
            let right = self.exclusive_or_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(token::T_VERTLINE, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// An exclusive or.
    fn exclusive_or_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.and_expr()?;
        while self.peek_kind() == Some(TokenKind::Chevron) {
            self.advance();
            let right = self.and_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(token::T_CHEVRON, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// An and.
    fn and_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.identical_expr()?;
        while self.peek_kind() == Some(TokenKind::Ampersand) {
            self.advance();
            let right = self.identical_expr()?;
            let position = self.position();
            let index =
                self.nodes
                    .make_np2(token::T_AMPERSAND, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// An identity test.
    fn identical_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.compare_expr()?;
        loop {
            let opcode = match self.peek_kind() {
                Some(TokenKind::NotEqual) => token::T_NOTEQUAL,
                Some(TokenKind::EqualEqual) => token::T_EQUALEQUAL,
                Some(TokenKind::DiscriminantNotEqual) => token::T_DISCNOTEQUAL,
                Some(TokenKind::DiscriminantEqual) => token::T_DISCEQUAL,
                _ => break,
            };
            self.advance();
            let right = self.compare_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(opcode, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// A comparison.
    fn compare_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.shift_expr()?;
        loop {
            let opcode = match self.peek_kind() {
                Some(TokenKind::LessThan) => token::T_LT,
                Some(TokenKind::GreaterThan) => token::T_GT,
                Some(TokenKind::LessOrEqual) => token::T_LTOREQUAL,
                Some(TokenKind::GreaterOrEqual) => token::T_GTOREQUAL,
                _ => break,
            };
            self.advance();
            let right = self.shift_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(opcode, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// A shift.
    fn shift_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.add_sub_expr()?;
        loop {
            let opcode = match self.peek_kind() {
                Some(TokenKind::RightArithmeticShift) => token::T_RARITHSHIFT,
                Some(TokenKind::LeftBitShift) => token::T_LARITHSHIFT,
                Some(TokenKind::RightBitShift) => token::T_RBITSHIFT,
                _ => break,
            };
            self.advance();
            let right = self.add_sub_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(opcode, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// An addition or a subtraction.
    fn add_sub_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.mul_div_expr()?;
        loop {
            let opcode = match self.peek_kind() {
                Some(TokenKind::Plus) => token::T_PLUS,
                Some(TokenKind::Minus) => token::T_MINUS,
                _ => break,
            };
            self.advance();
            let right = self.mul_div_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(opcode, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// A multiplication or a division.
    ///
    /// The multiplication takes the operand that follows it, and not the
    /// whole expression on its right.
    ///
    /// The star that ends an argument is not a multiplication: the token
    /// that follows the star tells the two apart.
    fn mul_div_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut node = self.unary_expr()?;
        loop {
            let opcode = match self.peek_kind() {
                Some(TokenKind::Percent) => token::T_PERCENT,
                Some(TokenKind::Slash) => token::T_SLASH,
                Some(TokenKind::Backslash) => token::T_BACKSLASH,
                Some(TokenKind::Asterisk) => {
                    if !self.operand_follows() {
                        break;
                    }
                    token::T_ASTERISK
                }
                _ => break,
            };
            self.advance();
            let right = self.unary_expr()?;
            let position = self.position();
            let index = self
                .nodes
                .make_np2(opcode, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// Whether the token that follows the star the parser looks at starts
    /// an operand.
    ///
    /// That token tells the multiplication from the star that ends an
    /// argument.
    fn operand_follows(&mut self) -> bool {
        self.fill(2);
        let Some(token) = self.lookahead.get(1) else {
            return false;
        };
        matches!(
            token.kind,
            TokenKind::ConstVal
                | TokenKind::Symbol
                | TokenKind::This
                | TokenKind::Super
                | TokenKind::Global
                | TokenKind::Void
                | TokenKind::Function
                | TokenKind::LeftBracket
                | TokenKind::Percent
                | TokenKind::Regexp
                | TokenKind::Slash
                | TokenKind::SlashAssign
                | TokenKind::LeftParenthesis
                | TokenKind::Dot
                | TokenKind::Exclamation
                | TokenKind::Tilde
                | TokenKind::Decrement
                | TokenKind::Increment
                | TokenKind::Invalidate
                | TokenKind::Delete
                | TokenKind::TypeOf
                | TokenKind::Sharp
                | TokenKind::Dollar
                | TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Ampersand
                | TokenKind::Asterisk
                | TokenKind::IsValid
                | TokenKind::Int
                | TokenKind::Real
                | TokenKind::String
                | TokenKind::New
        )
    }

    /// A unary expression.
    fn unary_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let opcode = match self.peek_kind() {
            Some(TokenKind::Exclamation) => Some(token::T_EXCRAMATION),
            Some(TokenKind::Tilde) => Some(token::T_TILDE),
            Some(TokenKind::Decrement) => Some(token::T_DECREMENT),
            Some(TokenKind::Increment) => Some(token::T_INCREMENT),
            Some(TokenKind::Invalidate) => Some(token::T_INVALIDATE),
            Some(TokenKind::Delete) => Some(token::T_DELETE),
            Some(TokenKind::TypeOf) => Some(token::T_TYPEOF),
            Some(TokenKind::Sharp) => Some(token::T_SHARP),
            Some(TokenKind::Dollar) => Some(token::T_DOLLAR),
            Some(TokenKind::Plus) => Some(token::T_UPLUS),
            Some(TokenKind::Minus) => Some(token::T_UMINUS),
            Some(TokenKind::Ampersand) => Some(token::T_IGNOREPROP),
            Some(TokenKind::Asterisk) => Some(token::T_PROPACCESS),
            Some(TokenKind::IsValid) => Some(token::T_ISVALID),
            Some(TokenKind::Int) => Some(token::T_INT),
            Some(TokenKind::Real) => Some(token::T_REAL),
            Some(TokenKind::String) => Some(token::T_STRING),
            _ => None,
        };
        if let Some(opcode) = opcode {
            self.advance();
            let operand = self.unary_expr()?;
            let position = self.position();
            let index = self.nodes.make_np1(opcode, position, Some(&operand));
            return Ok(self.nodes.node(index));
        }

        // the cast: the type that follows the parenthesis is read, so the
        // parser reads the token that follows it too
        if self.peek_kind() == Some(TokenKind::LeftParenthesis) && self.cast_ahead() {
            self.advance();
            let keyword = self.advance();
            let opcode = match keyword.map(|token| token.kind) {
                Some(TokenKind::Int) => token::T_INT,
                Some(TokenKind::Real) => token::T_REAL,
                _ => token::T_STRING,
            };
            self.expect(TokenKind::RightParenthesis)?;
            let operand = self.unary_expr()?;
            let position = self.position();
            let index = self.nodes.make_np1(opcode, position, Some(&operand));
            return Ok(self.nodes.node(index));
        }

        // the new operator takes a call
        if self.peek_kind() == Some(TokenKind::New) {
            self.advance();
            let index = self.func_call_expr()?;
            self.nodes.set_opcode(index, token::T_NEW);
            return Ok(self.nodes.node(index));
        }

        let node = self.incontextof_expr()?;
        match self.peek_kind() {
            Some(TokenKind::IsValid) => {
                self.advance();
                let position = self.position();
                let index = self.nodes.make_np1(token::T_ISVALID, position, Some(&node));
                Ok(self.nodes.node(index))
            }
            Some(TokenKind::InstanceOf) => {
                self.advance();
                let right = self.unary_expr()?;
                let position = self.position();
                let index =
                    self.nodes
                        .make_np2(token::T_INSTANCEOF, position, Some(&node), Some(&right));
                Ok(self.nodes.node(index))
            }
            Some(TokenKind::In) => {
                self.advance();
                let right = self.unary_expr()?;
                let position = self.position();
                let index = self
                    .nodes
                    .make_np2(token::T_IN, position, Some(&node), Some(&right));
                Ok(self.nodes.node(index))
            }
            _ => Ok(node),
        }
    }

    /// Whether the parenthesis the parser looks at opens a cast.
    ///
    /// The type that follows the parenthesis is read, and the parenthesis
    /// that follows the type: the cast is `(int)x`, and the type on its
    /// own is the operator that makes a string of its operand.
    ///
    /// The token that follows the parenthesis is the first of an
    /// expression, so a slash there starts a regular expression, and the
    /// analyser must read it as one.
    fn cast_ahead(&mut self) -> bool {
        let Some(token) = self.peek_second() else {
            return false;
        };
        if matches!(token.kind, TokenKind::Slash | TokenKind::SlashAssign) {
            self.read_regexp_again(1);
        }
        let Some(token) = self.peek_second() else {
            return false;
        };
        if !matches!(
            token.kind,
            TokenKind::Int | TokenKind::Real | TokenKind::String
        ) {
            return false;
        }
        let Some(token) = self.peek_third() else {
            return false;
        };
        if matches!(token.kind, TokenKind::Slash | TokenKind::SlashAssign) {
            self.read_regexp_again(2);
        }
        let Some(token) = self.peek_third() else {
            return false;
        };
        token.kind == TokenKind::RightParenthesis
    }

    /// An `incontextof` expression.
    fn incontextof_expr(&mut self) -> Result<Rc<Node>, ParseError> {
        let mut index = self.priority_expr()?;
        let mut node = self.nodes.node(index);
        while self.peek_kind() == Some(TokenKind::InContextOf) {
            self.advance();
            let right = self.incontextof_expr()?;
            let position = self.position();
            index = self
                .nodes
                .make_np2(token::T_INCONTEXTOF, position, Some(&node), Some(&right));
            node = self.nodes.node(index);
        }
        Ok(node)
    }

    /// A priority expression.
    fn priority_expr(&mut self) -> Result<usize, ParseError> {
        let index = self.priority_expr_primary()?;
        self.priority_expr_tail(index)
    }

    /// The operators that follow a priority expression.
    fn priority_expr_tail(&mut self, first: usize) -> Result<usize, ParseError> {
        let mut index = first;
        loop {
            match self.peek_kind() {
                Some(TokenKind::LeftBracket) => {
                    self.advance();
                    let bracket = self.expr()?;
                    self.expect(TokenKind::RightBracket)?;
                    let position = self.position();
                    index = self.nodes.make_np2(
                        token::T_LBRACKET,
                        position,
                        Some(&self.nodes.node(index)),
                        Some(&bracket),
                    );
                }
                Some(TokenKind::Dot) => {
                    self.advance();
                    self.lexer.expect_bare_word();
                    let token = self.expect(TokenKind::Symbol)?;
                    let position = self.position();
                    let name = self.nodes.make_np0(token::T_CONSTVAL, position);
                    let value = Value::String(self.name_of(&token));
                    self.nodes.set_value(name, value);
                    index = self.nodes.make_np2(
                        token::T_DOT,
                        position,
                        Some(&self.nodes.node(index)),
                        Some(&self.nodes.node(name)),
                    );
                }
                Some(TokenKind::Increment) => {
                    self.advance();
                    let position = self.position();
                    index = self.nodes.make_np1(
                        token::T_POSTINCREMENT,
                        position,
                        Some(&self.nodes.node(index)),
                    );
                }
                Some(TokenKind::Decrement) => {
                    self.advance();
                    let position = self.position();
                    index = self.nodes.make_np1(
                        token::T_POSTDECREMENT,
                        position,
                        Some(&self.nodes.node(index)),
                    );
                }
                Some(TokenKind::Exclamation) => {
                    self.advance();
                    let position = self.position();
                    index =
                        self.nodes
                            .make_np1(token::T_EVAL, position, Some(&self.nodes.node(index)));
                }
                Some(TokenKind::LeftParenthesis) => {
                    self.advance();
                    let arguments = self.call_arg_list()?;
                    self.expect(TokenKind::RightParenthesis)?;
                    let position = self.position();
                    index = self.nodes.make_np2(
                        token::T_LPARENTHESIS,
                        position,
                        Some(&self.nodes.node(index)),
                        Some(&arguments),
                    );
                }
                _ => break,
            }
        }
        Ok(index)
    }

    /// The first part of a priority expression.
    fn priority_expr_primary(&mut self) -> Result<usize, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::LeftParenthesis) => {
                self.advance();
                if self.peek_kind() == Some(TokenKind::Const) {
                    // the `(const)` form: the constant array or the
                    // constant dictionary
                    return self.const_inline();
                }
                let node = self.expr()?;
                self.expect(TokenKind::RightParenthesis)?;
                Ok(self.node_index(&node))
            }
            Some(TokenKind::Dot) => {
                self.advance();
                self.lexer.expect_bare_word();
                let token = self.expect(TokenKind::Symbol)?;
                let position = self.position();
                let name = self.nodes.make_np0(token::T_CONSTVAL, position);
                let value = Value::String(self.name_of(&token));
                self.nodes.set_value(name, value);
                Ok(self
                    .nodes
                    .make_np1(token::T_WITHDOT, position, Some(&self.nodes.node(name))))
            }
            _ => self.factor_expr(),
        }
    }

    /// The place a node has in the tree the parser made.
    fn node_index(&self, node: &Rc<Node>) -> usize {
        self.nodes
            .all()
            .iter()
            .position(|made| Rc::ptr_eq(made, node))
            .unwrap_or(0)
    }

    /// A function call.
    ///
    /// The call takes a whole priority expression on its left, so the call
    /// of `new System.Foo()` is the call of the member access.
    fn func_call_expr(&mut self) -> Result<usize, ParseError> {
        let index = self.priority_expr()?;
        Ok(index)
    }

    /// The arguments of a call.
    fn call_arg_list(&mut self) -> Result<Rc<Node>, ParseError> {
        if self.peek_kind() == Some(TokenKind::Omit) {
            self.advance();
            let position = self.position();
            let index = self.nodes.make_np0(token::T_OMIT, position);
            return Ok(self.nodes.node(index));
        }
        let first = self.call_arg()?;
        // the token that follows the argument is read before the node that
        // holds it is made
        if self.peek_kind() != Some(TokenKind::Comma) {
            let position = self.position();
            let first = self.call_arg_node(first, position);
            let index = self.nodes.make_np1(token::T_ARG, position, first.as_ref());
            return Ok(self.nodes.node(index));
        }
        let position = self.position();
        let first = self.call_arg_node(first, position);
        let mut index = self.nodes.make_np1(token::T_ARG, position, first.as_ref());
        loop {
            self.advance();
            let argument = self.call_arg()?;
            let more = self.peek_kind() == Some(TokenKind::Comma);
            let position = self.position();
            let argument = self.call_arg_node(argument, position);
            index = self.nodes.make_np2(
                token::T_ARG,
                position,
                argument.as_ref(),
                Some(&self.nodes.node(index)),
            );
            if !more {
                break;
            }
        }
        Ok(self.nodes.node(index))
    }

    /// The node one argument of a call makes.
    ///
    /// The token that follows the argument is read before the argument is
    /// reduced, so the node the argument makes is placed after that
    /// token.
    fn call_arg_node(&mut self, argument: CallArg, position: i32) -> Option<Rc<Node>> {
        match argument {
            CallArg::Empty => None,
            CallArg::ExpandStar => {
                let index = self.nodes.make_np1(token::T_EXPANDARG, position, None);
                Some(self.nodes.node(index))
            }
            CallArg::Expand(node) => {
                let index = self
                    .nodes
                    .make_np1(token::T_EXPANDARG, position, Some(&node));
                Some(self.nodes.node(index))
            }
            CallArg::Node(node) => Some(node),
        }
    }

    /// One argument of a call.
    fn call_arg(&mut self) -> Result<CallArg, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::RightParenthesis) | Some(TokenKind::Comma) => Ok(CallArg::Empty),
            Some(TokenKind::Asterisk) => {
                self.advance();
                Ok(CallArg::ExpandStar)
            }
            _ => {
                let node = self.expr_no_comma()?;
                if self.peek_kind() == Some(TokenKind::Asterisk) {
                    // the star that ends the argument
                    self.advance();
                    return Ok(CallArg::Expand(node));
                }
                Ok(CallArg::Node(node))
            }
        }
    }

    /// A factor.
    fn factor_expr(&mut self) -> Result<usize, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::ConstVal) => {
                let token = self.advance().unwrap_or(Token::new(TokenKind::ConstVal));
                let position = self.position();
                let index = self.nodes.make_np0(token::T_CONSTVAL, position);
                let value = self.value_of(&token);
                self.nodes.set_value(index, value);
                Ok(index)
            }
            Some(TokenKind::Symbol) => {
                let token = self.advance().unwrap_or(Token::new(TokenKind::Symbol));
                let position = self.position();
                let index = self.nodes.make_np0(token::T_SYMBOL, position);
                let value = Value::String(self.name_of(&token));
                self.nodes.set_value(index, value);
                Ok(index)
            }
            Some(TokenKind::This) => {
                self.advance();
                let position = self.position();
                Ok(self.nodes.make_np0(token::T_THIS, position))
            }
            Some(TokenKind::Super) => {
                self.advance();
                let position = self.position();
                Ok(self.nodes.make_np0(token::T_SUPER, position))
            }
            Some(TokenKind::Global) => {
                self.advance();
                let position = self.position();
                Ok(self.nodes.make_np0(token::T_GLOBAL, position))
            }
            Some(TokenKind::Void) => {
                self.advance();
                let position = self.position();
                Ok(self.nodes.make_np0(token::T_VOID, position))
            }
            Some(TokenKind::Function) => self.func_expr_def(),
            Some(TokenKind::LeftBracket) => self.inline_array(),
            Some(TokenKind::Percent) => self.inline_dic(),
            Some(TokenKind::Regexp) => {
                let token = self.advance().unwrap_or(Token::new(TokenKind::Regexp));
                let position = self.position();
                let index = self.nodes.make_np0(token::T_REGEXP, position);
                let value = self.value_of(&token);
                self.nodes.set_value(index, value);
                Ok(index)
            }
            Some(TokenKind::Slash) | Some(TokenKind::SlashAssign) => {
                // a regular expression: the analyser reads it after the
                // slash that starts it
                self.lexer.expect_regexp();
                self.advance();
                let token = self.advance().unwrap_or(Token::new(TokenKind::Regexp));
                let position = self.position();
                let index = self.nodes.make_np0(token::T_REGEXP, position);
                let value = self.value_of(&token);
                self.nodes.set_value(index, value);
                Ok(index)
            }
            _ => Err(self.syntax_error("a value")),
        }
    }

    /// The value a token carries.
    fn value_of(&self, token: &Token) -> Value {
        match self.lexer.value(token) {
            Some(ValueRef::Null) => Value::Object(ObjectId::NULL),
            Some(ValueRef::Integer(value)) => Value::Integer(value),
            Some(ValueRef::Real(value)) => Value::Real(value),
            Some(ValueRef::String(units)) => Value::String(String::from_utf16_lossy(units)),
            Some(ValueRef::Octet(bytes)) => Value::Octet(bytes.to_vec()),
            None => Value::Void,
        }
    }

    /// A function expression.
    fn func_expr_def(&mut self) -> Result<usize, ParseError> {
        self.advance();
        let units: Vec<u16> = "(anonymous)".encode_utf16().collect();
        let context = self
            .commands
            .push_context(Some(&units), ContextType::ExprFunction);
        self.commands.enter_block();
        let result = (|| {
            self.func_decl_arg_opt()?;
            self.variable_type()?;
            self.block()
        })();
        self.commands.exit_block();
        self.commands.pop_context()?;
        result?;
        let position = self.position();
        let index = self.nodes.make_np0(token::T_CONSTVAL, position);
        // the value of the expression is the context the function was
        // compiled into; the context is named by its place in the block
        self.nodes.set_value(index, Value::Context(context));
        Ok(index)
    }

    /// An inline array.
    fn inline_array(&mut self) -> Result<usize, ParseError> {
        self.advance();
        let position = self.position();
        let index = self.nodes.make_np0(token::T_INLINEARRAY, position);
        self.current_nodes.push(index);
        let result = self.array_elm_list();
        self.current_nodes.pop();
        result?;
        self.expect(TokenKind::RightBracket)?;
        Ok(index)
    }

    /// The elements of an inline array.
    fn array_elm_list(&mut self) -> Result<(), ParseError> {
        loop {
            let element = self.array_elm()?;
            if let Some(current) = self.current_nodes.last().copied() {
                self.nodes.add_child(current, element);
            }
            if self.peek_kind() == Some(TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Ok(())
    }

    /// One element of an inline array.
    fn array_elm(&mut self) -> Result<Rc<Node>, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::RightBracket) | Some(TokenKind::Comma) => {
                let position = self.position();
                let index = self.nodes.make_np1(token::T_ARRAYARG, position, None);
                Ok(self.nodes.node(index))
            }
            _ => {
                let node = self.expr_no_comma()?;
                let position = self.position();
                let index = self
                    .nodes
                    .make_np1(token::T_ARRAYARG, position, Some(&node));
                Ok(self.nodes.node(index))
            }
        }
    }

    /// Whether the name the parser looks at is followed by a `:`.
    ///
    /// The token that follows the name tells the two forms of the element
    /// apart.
    fn name_has_colon(&mut self) -> bool {
        self.fill(2);
        self.lookahead
            .get(1)
            .is_some_and(|token| token.kind == TokenKind::Colon)
    }

    /// A constant inline array or a constant inline dictionary.
    ///
    /// The `(const)` form is the one a live object is built for. The
    /// `const` is read and the `)` is expected; the array and the
    /// dictionary follow it.
    fn const_inline(&mut self) -> Result<usize, ParseError> {
        self.advance();
        self.expect(TokenKind::RightParenthesis)?;
        // the grammar makes the node after the bracket is read, so the
        // node's place is the one just past the bracket
        let dictionary = match self.peek_kind() {
            Some(TokenKind::LeftBracket) => false,
            Some(TokenKind::Percent) => {
                self.advance();
                true
            }
            _ => return Err(self.syntax_error("'[' or '%'")),
        };
        self.expect(TokenKind::LeftBracket)?;
        let position = self.position();
        let index = self.nodes.make_np0(token::T_CONSTVAL, position);
        // the object the grammar builds is one object, and two constant
        // arrays are two objects: the identity tells them apart
        let identity = ObjectId(index as u64);
        if dictionary {
            let entries = self.const_dic_elm_list()?;
            self.expect(TokenKind::RightBracket)?;
            self.nodes
                .set_value(index, Value::ConstDictionary { identity, entries });
        } else {
            let elements = self.const_array_elm_list()?;
            self.expect(TokenKind::RightBracket)?;
            self.nodes
                .set_value(index, Value::ConstArray { identity, elements });
        }
        Ok(index)
    }

    /// The elements of a constant inline array.
    fn const_array_elm_list(&mut self) -> Result<Vec<Value>, ParseError> {
        let mut elements = Vec::new();
        if self.peek_kind() == Some(TokenKind::RightBracket) {
            return Ok(elements);
        }
        loop {
            elements.push(self.const_array_elm()?);
            if self.peek_kind() != Some(TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        Ok(elements)
    }

    /// One element of a constant inline array.
    ///
    /// The element is a constant value, a sign and a constant value, a
    /// value of no value, or a constant array or a constant dictionary.
    fn const_array_elm(&mut self) -> Result<Value, ParseError> {
        match self.peek_kind() {
            Some(TokenKind::Minus) => {
                self.advance();
                let value = self.const_value()?;
                self.signed_value(value, token::T_UMINUS)
            }
            Some(TokenKind::Plus) => {
                self.advance();
                let value = self.const_value()?;
                self.signed_value(value, token::T_UPLUS)
            }
            Some(TokenKind::Void) => {
                self.advance();
                Ok(Value::Void)
            }
            Some(TokenKind::LeftParenthesis) => {
                // a nested constant array or constant dictionary
                self.advance();
                if self.peek_kind() != Some(TokenKind::Const) {
                    return Err(self.syntax_error("'const'"));
                }
                let node = self.const_inline()?;
                Ok(self
                    .nodes
                    .node(node)
                    .value()
                    .cloned()
                    .unwrap_or(Value::Void))
            }
            _ => self.const_value(),
        }
    }

    /// The entries of a constant inline dictionary.
    fn const_dic_elm_list(&mut self) -> Result<Vec<(Value, Value)>, ParseError> {
        let mut entries = Vec::new();
        if self.peek_kind() == Some(TokenKind::RightBracket) {
            return Ok(entries);
        }
        loop {
            entries.push(self.const_dic_elm()?);
            if self.peek_kind() != Some(TokenKind::Comma) {
                break;
            }
            self.advance();
        }
        Ok(entries)
    }

    /// One entry of a constant inline dictionary.
    ///
    /// The entry is the name of the member and the value it holds, and
    /// the two are told apart by the `,` that follows the name.
    fn const_dic_elm(&mut self) -> Result<(Value, Value), ParseError> {
        let name = self.const_value()?;
        self.expect(TokenKind::Comma)?;
        let value = match self.peek_kind() {
            Some(TokenKind::Minus) => {
                self.advance();
                let value = self.const_value()?;
                self.signed_value(value, token::T_UMINUS)?
            }
            Some(TokenKind::Plus) => {
                self.advance();
                let value = self.const_value()?;
                self.signed_value(value, token::T_UPLUS)?
            }
            Some(TokenKind::Void) => {
                self.advance();
                Value::Void
            }
            Some(TokenKind::LeftParenthesis) => {
                // a nested constant array or constant dictionary
                self.advance();
                if self.peek_kind() != Some(TokenKind::Const) {
                    return Err(self.syntax_error("'const'"));
                }
                let node = self.const_inline()?;
                self.nodes
                    .node(node)
                    .value()
                    .cloned()
                    .unwrap_or(Value::Void)
            }
            _ => self.const_value()?,
        };
        Ok((name, value))
    }

    /// The value of a constant token.
    fn const_value(&mut self) -> Result<Value, ParseError> {
        let token = self.expect(TokenKind::ConstVal)?;
        Ok(self.value_of(&token))
    }

    /// The value with the sign the grammar puts on it.
    ///
    /// The sign is the variant's own: the minus changes the sign and the
    /// plus reads the value as a number.
    fn signed_value(&mut self, value: Value, opcode: i32) -> Result<Value, ParseError> {
        match crate::vm::codegen::folding::fold_unary(opcode, &value) {
            Some(folded) => Ok(folded),
            None => Err(self.syntax_error("a value the sign can be put on")),
        }
    }

    /// An inline dictionary.
    fn inline_dic(&mut self) -> Result<usize, ParseError> {
        self.advance();
        self.expect(TokenKind::LeftBracket)?;
        let position = self.position();
        let index = self.nodes.make_np0(token::T_INLINEDIC, position);
        self.current_nodes.push(index);
        let result = self.dic_elm_list();
        self.current_nodes.pop();
        result?;
        if self.peek_kind() == Some(TokenKind::Comma) {
            self.advance();
        }
        self.expect(TokenKind::RightBracket)?;
        Ok(index)
    }

    /// The elements of an inline dictionary.
    fn dic_elm_list(&mut self) -> Result<(), ParseError> {
        if matches!(
            self.peek_kind(),
            Some(TokenKind::RightBracket) | Some(TokenKind::Comma)
        ) {
            return Ok(());
        }
        loop {
            let element = self.dic_elm()?;
            if let Some(current) = self.current_nodes.last().copied() {
                self.nodes.add_child(current, element);
            }
            if self.peek_kind() == Some(TokenKind::Comma) {
                self.advance();
                if self.peek_kind() == Some(TokenKind::RightBracket) {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    /// One element of an inline dictionary.
    ///
    /// The element is a name and a value, and the name and the value are
    /// told apart by the `:` that follows the name.
    fn dic_elm(&mut self) -> Result<Rc<Node>, ParseError> {
        if self.peek_kind() == Some(TokenKind::Symbol) && self.name_has_colon() {
            let token = self.advance().unwrap_or(Token::new(TokenKind::Symbol));
            self.expect(TokenKind::Colon)?;
            let value = self.expr_no_comma()?;
            let position = self.position();
            let name = self.nodes.make_np0(token::T_CONSTVAL, position);
            let text = self.name_of(&token);
            self.nodes.set_value(name, Value::String(text));
            let index = self.nodes.make_np2(
                token::T_DICELM,
                position,
                Some(&self.nodes.node(name)),
                Some(&value),
            );
            return Ok(self.nodes.node(index));
        }
        let name = self.expr_no_comma()?;
        self.expect(TokenKind::Comma)?;
        let value = self.expr_no_comma()?;
        let position = self.position();
        let index = self
            .nodes
            .make_np2(token::T_DICELM, position, Some(&name), Some(&value));
        Ok(self.nodes.node(index))
    }
}
