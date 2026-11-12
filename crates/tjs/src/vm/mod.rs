//! The Rust VM: the script columns that replace the vendored ones.
//!
//! The script language is the TJS2 dialect the games are written in. This
//! module holds its front end in Rust: the lexical analyser that turns
//! script text into tokens, the parser that turns the tokens into an
//! expression tree, and the code generator that turns the tree into the
//! intermediate code the VM executes.
//!
//! These columns replace the vendored tree beside them, in
//! `crates/tjs/cpp/tjs2/`: the analyser, the grammar and the
//! generator there are what this module is written against. The C interfaces
//! the vendored tree calls during the replacement are in each column's
//! own `ffi` module.

#![deny(unsafe_code)]

pub mod codegen;
pub mod commands;
pub mod lexer;
pub mod parser;
pub mod script;

pub use codegen::Generator;
pub use lexer::{LexError, Lexer, PpValues, Token, TokenKind, ValueRef};
pub use parser::{Commands, ParseError, Parsed, Parser};
