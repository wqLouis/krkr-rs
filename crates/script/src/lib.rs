//! krkr-rs **L4 script language**.
//!
//! The script language is the TJS2 dialect the games are written in. This
//! crate holds its front end: the lexical analyser that turns script text
//! into tokens, and the C interface the vendored analyser is replaced
//! through.
//!
//! The crate is independent of the vendored tree and of the compatibility
//! layer, so both the host and the vendored tree can use it without a cycle.

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub mod ffi;
pub mod lexer;

pub use lexer::{LexError, Lexer, PpValues, Token, TokenKind, ValueRef};
