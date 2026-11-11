//! The token vocabulary of the script language.

/// A token kind.
///
/// The discriminants are the numbers the grammar gives the tokens, so a
/// token kind can be compared with the grammar's own numbering without a
/// translation table. The grammar-internal kinds — the ones the parser
/// produces and the lexer never returns — are not part of this set.
///
/// [`TokenKind::grammar_code`] returns the number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum TokenKind {
    /// `,`
    Comma = 258,
    /// `=`
    Equal = 259,
    /// `&=`
    AmpersandAssign = 260,
    /// `|=`
    VerticalBarAssign = 261,
    /// `^=`
    ChevronAssign = 262,
    /// `-=`
    MinusAssign = 263,
    /// `+=`
    PlusAssign = 264,
    /// `%=`
    PercentAssign = 265,
    /// `/=`
    SlashAssign = 266,
    /// `\=`
    BackslashAssign = 267,
    /// `*=`
    AsteriskAssign = 268,
    /// `||=`
    LogicalOrAssign = 269,
    /// `&&=`
    LogicalAndAssign = 270,
    /// `>>>=`
    RightBitShiftAssign = 271,
    /// `<<=`
    LeftBitShiftAssign = 272,
    /// `>>=`
    RightArithmeticShiftAssign = 273,
    /// `?`
    Question = 274,
    /// `||`
    LogicalOr = 275,
    /// `&&`
    LogicalAnd = 276,
    /// `|`
    VerticalBar = 277,
    /// `^`
    Chevron = 278,
    /// `&`
    Ampersand = 279,
    /// `!=`
    NotEqual = 280,
    /// `==`
    EqualEqual = 281,
    /// `!==`
    DiscriminantNotEqual = 282,
    /// `===`
    DiscriminantEqual = 283,
    /// `<->`
    Swap = 284,
    /// `<`
    LessThan = 285,
    /// `>`
    GreaterThan = 286,
    /// `<=`
    LessOrEqual = 287,
    /// `>=`
    GreaterOrEqual = 288,
    /// `>>`
    RightArithmeticShift = 289,
    /// `<<`
    LeftBitShift = 290,
    /// `>>>`
    RightBitShift = 291,
    /// `%`
    Percent = 292,
    /// `/`
    Slash = 293,
    /// `\`
    Backslash = 294,
    /// `*`
    Asterisk = 295,
    /// `!`
    Exclamation = 296,
    /// `~`
    Tilde = 297,
    /// `--`
    Decrement = 298,
    /// `++`
    Increment = 299,
    /// `new`
    New = 300,
    /// `delete`
    Delete = 301,
    /// `typeof`
    TypeOf = 302,
    /// `+`
    Plus = 303,
    /// `-`
    Minus = 304,
    /// `#`
    Sharp = 305,
    /// `$`
    Dollar = 306,
    /// `isvalid`
    IsValid = 307,
    /// `invalidate`
    Invalidate = 308,
    /// `instanceof`
    InstanceOf = 309,
    /// `(`
    LeftParenthesis = 310,
    /// `.`
    Dot = 311,
    /// `[`
    LeftBracket = 312,
    /// `this`
    This = 313,
    /// `super`
    Super = 314,
    /// `global`
    Global = 315,
    /// `]`
    RightBracket = 316,
    /// `class`
    Class = 317,
    /// `)`
    RightParenthesis = 318,
    /// `:`
    Colon = 319,
    /// `;`
    Semicolon = 320,
    /// `{`
    LeftBrace = 321,
    /// `}`
    RightBrace = 322,
    /// `continue`
    Continue = 323,
    /// `function`
    Function = 324,
    /// `debugger`
    Debugger = 325,
    /// `default`
    Default = 326,
    /// `case`
    Case = 327,
    /// `extends`
    Extends = 328,
    /// `finally`
    Finally = 329,
    /// `property`
    Property = 330,
    /// `private`
    Private = 331,
    /// `public`
    Public = 332,
    /// `protected`
    Protected = 333,
    /// `static`
    Static = 334,
    /// `return`
    Return = 335,
    /// `break`
    Break = 336,
    /// `export`
    Export = 337,
    /// `import`
    Import = 338,
    /// `switch`
    Switch = 339,
    /// `in`
    In = 340,
    /// `incontextof`
    InContextOf = 341,
    /// `for`
    For = 342,
    /// `while`
    While = 343,
    /// `do`
    Do = 344,
    /// `if`
    If = 345,
    /// `var`
    Var = 346,
    /// `const`
    Const = 347,
    /// `enum`
    Enum = 348,
    /// `goto`
    Goto = 349,
    /// `throw`
    Throw = 350,
    /// `try`
    Try = 351,
    /// `setter`
    Setter = 352,
    /// `getter`
    Getter = 353,
    /// `else`
    Else = 354,
    /// `catch`
    Catch = 355,
    /// `...`
    Omit = 356,
    /// `synchronized`
    Synchronized = 357,
    /// `with`
    With = 358,
    /// `int`
    Int = 359,
    /// `real`
    Real = 360,
    /// `string`
    String = 361,
    /// `octet`
    Octet = 362,
    /// `false`
    False = 363,
    /// `null`
    Null = 364,
    /// `true`
    True = 365,
    /// `void`
    Void = 366,
    /// `NaN`
    NaN = 367,
    /// `Infinity`
    Infinity = 368,
    /// A literal value: a number, a string, a regular expression or a
    /// binary buffer.
    ConstVal = 385,
    /// A name.
    Symbol = 386,
    /// A regular expression literal.
    Regexp = 387,
}

impl TokenKind {
    /// The number the grammar gives this token kind.
    ///
    /// The number is the kind's discriminant, so a token kind can be
    /// compared with the grammar's own numbering.
    pub fn grammar_code(self) -> u16 {
        self as u16
    }
}

/// One token: its kind and, for the kinds that carry one, its value.
///
/// The value is named by its index in the analyser's values, so a token
/// costs eight bytes and producing one costs no allocation. The value
/// itself is read through the analyser that produced the token, with
/// [`Lexer::value`](super::Lexer::value).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Token {
    /// The token's kind.
    pub kind: TokenKind,
    /// The index of the token's value in the analyser's values, when the
    /// token carries one.
    pub value: Option<u32>,
}

impl Token {
    /// A token without a value.
    pub fn new(kind: TokenKind) -> Self {
        Self { kind, value: None }
    }

    /// A token that carries a value.
    pub fn with_value(kind: TokenKind, value: u32) -> Self {
        Self {
            kind,
            value: Some(value),
        }
    }
}
