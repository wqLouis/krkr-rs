//! The operator table.
//!
//! The operators are matched by a generated scanner: the table is the
//! longest match over the text at the current position, so the order of
//! the patterns does not matter and a new operator is one line.
//!
//! The table holds the operators that are two characters or more. The
//! single character operators are read by the analyser itself, because the
//! characters that start a comment, a number or a binary literal are
//! checked before the table.

use logos::Logos;

use super::TokenKind;

/// The tokens the generated scanner recognises.
#[derive(Logos, Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scaffold {
    /// An operator.
    #[regex(br">>>=", |_| TokenKind::RightBitShiftAssign)]
    #[regex(br">>>", |_| TokenKind::RightBitShift)]
    #[regex(br">>=", |_| TokenKind::RightArithmeticShiftAssign)]
    #[regex(br">>", |_| TokenKind::RightArithmeticShift)]
    #[regex(br">=", |_| TokenKind::GreaterOrEqual)]
    #[regex(br"<<=", |_| TokenKind::LeftBitShiftAssign)]
    #[regex(br"<->", |_| TokenKind::Swap)]
    #[regex(br"<=", |_| TokenKind::LessOrEqual)]
    #[regex(br"<<", |_| TokenKind::LeftBitShift)]
    #[regex(br"===", |_| TokenKind::DiscriminantEqual)]
    #[regex(br"==", |_| TokenKind::EqualEqual)]
    #[regex(br"=>", |_| TokenKind::Comma)]
    #[regex(br"!==", |_| TokenKind::DiscriminantNotEqual)]
    #[regex(br"!=", |_| TokenKind::NotEqual)]
    #[regex(br"&&=", |_| TokenKind::LogicalAndAssign)]
    #[regex(br"&&", |_| TokenKind::LogicalAnd)]
    #[regex(br"&=", |_| TokenKind::AmpersandAssign)]
    #[regex(br"\|\|=", |_| TokenKind::LogicalOrAssign)]
    #[regex(br"\|\|", |_| TokenKind::LogicalOr)]
    #[regex(br"\|=", |_| TokenKind::VerticalBarAssign)]
    #[regex(br"\.\.\.", |_| TokenKind::Omit)]
    #[regex(br"\+\+", |_| TokenKind::Increment)]
    #[regex(br"\+=", |_| TokenKind::PlusAssign)]
    #[regex(br"-=", |_| TokenKind::MinusAssign)]
    #[regex(br"--", |_| TokenKind::Decrement)]
    #[regex(br"\*=", |_| TokenKind::AsteriskAssign)]
    #[regex(br"/=", |_| TokenKind::SlashAssign)]
    #[regex(br"\\=", |_| TokenKind::BackslashAssign)]
    #[regex(br"%=", |_| TokenKind::PercentAssign)]
    #[regex(br"\^=", |_| TokenKind::ChevronAssign)]
    Operator(TokenKind),
}
