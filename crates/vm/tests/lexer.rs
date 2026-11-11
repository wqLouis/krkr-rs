//! The lexical analyser's behaviour.
//!
//! Every assertion below cites the place in the vendored lexer the
//! behaviour comes from.

use vm::lexer::{Lexer, Literal, PpValues, ScriptString, Token, TokenKind};

/// The tokens of a script.
fn lex(source: &str) -> Vec<Token> {
    let units: Vec<u16> = source.encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token().expect("the script lexes") {
        tokens.push(token);
    }
    tokens
}

/// The kinds of the tokens of a script.
fn kinds(source: &str) -> Vec<TokenKind> {
    lex(source).into_iter().map(|token| token.kind).collect()
}

/// The value of the first token that carries one.
fn value(source: &str) -> Literal {
    lex(source)
        .into_iter()
        .find_map(|token| token.value)
        .expect("the token carries a value")
}

/// The text of a string value.
fn text(value: &Literal) -> String {
    match value {
        Literal::String(string) => String::from_utf16_lossy(string.units()),
        other => panic!("the value is not a string: {other:?}"),
    }
}

#[test]
fn an_operator_is_the_longest_one_that_matches() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1287-1300 — the operators are
    // matched from the longest to the shortest.
    assert_eq!(kinds(">>>="), vec![TokenKind::RightBitShiftAssign]);
    assert_eq!(kinds(">>>"), vec![TokenKind::RightBitShift]);
    assert_eq!(kinds(">>="), vec![TokenKind::RightArithmeticShiftAssign]);
    assert_eq!(kinds(">="), vec![TokenKind::GreaterOrEqual]);
    assert_eq!(kinds(">"), vec![TokenKind::GreaterThan]);
}

#[test]
fn the_swap_operator_is_two_characters() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1302 — `<->` is the swap operator.
    assert_eq!(kinds("<->"), vec![TokenKind::Swap]);
}

#[test]
fn the_arrow_is_a_comma() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1330 — `=>` stands for a comma.
    assert_eq!(kinds("=>"), vec![TokenKind::Comma]);
}

#[test]
fn a_name_is_a_symbol() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1497-1512 — a word that is not a
    // reserved word is a symbol.
    let tokens = lex("name");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, TokenKind::Symbol);
    assert_eq!(text(tokens[0].value.as_ref().unwrap()), "name");
}

#[test]
fn a_reserved_word_is_its_own_kind() {
    // reference/cpp/core/tjs2/tjsLex.cpp:960-990 — the reserved words and
    // the kinds they stand for.
    assert_eq!(kinds("function"), vec![TokenKind::Function]);
    assert_eq!(kinds("incontextof"), vec![TokenKind::InContextOf]);
    assert_eq!(kinds("synchronized"), vec![TokenKind::Synchronized]);
    assert_eq!(kinds("int"), vec![TokenKind::Int]);
}

#[test]
fn the_keywords_that_carry_a_value_carry_it() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1521-1548 — `false`, `true`,
    // `null`, `NaN` and `Infinity` carry a value.
    assert_eq!(value("false"), Literal::Integer(0));
    assert_eq!(value("true"), Literal::Integer(1));
    assert_eq!(value("null"), Literal::Null);
    assert!(matches!(value("NaN"), Literal::Real(number) if number.is_nan()));
    assert_eq!(value("Infinity"), Literal::Real(f64::INFINITY));
}

#[test]
fn a_name_may_hold_a_character_that_is_not_ascii() {
    // reference/cpp/core/tjs2/tjsLex.cpp:52-57 — every code unit outside
    // the ASCII range is a letter, so it is part of a name.
    let tokens = lex("日本語");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].kind, TokenKind::Symbol);
    assert_eq!(text(tokens[0].value.as_ref().unwrap()), "日本語");
}

#[test]
fn a_decimal_number_is_an_integer() {
    // reference/cpp/core/tjs2/tjsLex.cpp:693-699 — a decimal number with
    // no fractional and no exponent part is an integer.
    assert_eq!(value("123"), Literal::Integer(123));
    assert_eq!(value("0"), Literal::Integer(0));
}

#[test]
fn a_number_that_starts_with_zero_is_octal() {
    // reference/cpp/core/tjs2/tjsLex.cpp:684-689 — a number that starts
    // with a zero and no other marker is octal.
    assert_eq!(value("0123"), Literal::Integer(83));
    assert_eq!(value("0"), Literal::Integer(0));
}

#[test]
fn a_hexadecimal_number_is_read_in_base_sixteen() {
    // reference/cpp/core/tjs2/tjsLex.cpp:666-670 — `0x` starts a
    // hexadecimal number.
    assert_eq!(value("0x1f"), Literal::Integer(31));
    assert_eq!(value("0X1F"), Literal::Integer(31));
}

#[test]
fn a_binary_number_is_read_in_base_two() {
    // reference/cpp/core/tjs2/tjsLex.cpp:671-675 — `0b` starts a binary
    // number.
    assert_eq!(value("0b1011"), Literal::Integer(11));
}

#[test]
fn a_number_with_a_fraction_is_a_real() {
    // reference/cpp/core/tjs2/tjsLex.cpp:690-693 — a number with a
    // fractional part is a real.
    assert_eq!(value("1.5"), Literal::Real(1.5));
    assert_eq!(value(".5"), Literal::Real(0.5));
    assert_eq!(value("1e3"), Literal::Real(1000.0));
    assert_eq!(value("1.5e-2"), Literal::Real(0.015));
}

#[test]
fn an_exponent_without_digits_is_the_number_before_it() {
    // reference/cpp/core/tjs2/tjsLex.cpp:404-430 — the exponent is read
    // while it has digits, so `1e` is the number `1`.
    assert_eq!(value("1e"), Literal::Real(1.0));
}

#[test]
fn a_string_holds_its_text() {
    // reference/cpp/core/tjs2/tjsLex.cpp:254-366 — a string literal is the
    // text between the delimiters.
    assert_eq!(text(&value("\"hello\"")), "hello");
    assert_eq!(text(&value("'hello'")), "hello");
}

#[test]
fn an_escape_is_decoded() {
    // reference/cpp/core/tjs2/tjsLex.cpp:270-330 — the escapes of a
    // string literal.
    assert_eq!(text(&value(r#""a\tb""#)), "a\tb");
    assert_eq!(text(&value(r#""a\nb""#)), "a\nb");
    assert_eq!(text(&value(r#""a\qb""#)), "aqb");
    assert_eq!(text(&value(r#""\x41""#)), "A");
    assert_eq!(text(&value(r#""\0101""#)), "A");
    assert_eq!(text(&value(r#""\101""#)), "101");
}

#[test]
fn two_string_literals_are_one() {
    // reference/cpp/core/tjs2/tjsLex.cpp:313-328 — a literal that is
    // followed by another literal of the same delimiter is one literal.
    assert_eq!(text(&value("\"ab\" \"cd\"")), "abcd");
}

#[test]
fn a_regular_expression_is_the_flags_and_the_pattern() {
    // reference/cpp/core/tjs2/tjsLex.cpp:822-880 — the internal form of a
    // regular expression is the flags and the pattern between slashes.
    let units: Vec<u16> = "/abc/g".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    // the parser tells the analyser that the next token is a regular
    // expression
    // reference/cpp/core/tjs2/tjsLex.cpp:1877-1884 — the notification.
    lexer.expect_regexp();
    let token = lexer.next_token().expect("the script lexes").unwrap();
    assert_eq!(token.kind, TokenKind::Regexp);
    assert_eq!(text(token.value.as_ref().unwrap()), "//g/abc");
}

#[test]
fn a_binary_literal_holds_its_bytes() {
    // reference/cpp/core/tjs2/tjsLex.cpp:734-820 — a binary literal is the
    // hexadecimal digits between `<%` and `%>`.
    assert_eq!(value("<% 12,34 %>"), Literal::Octet(vec![0x12, 0x34]));
    assert_eq!(value("<%1234%>"), Literal::Octet(vec![0x12, 0x34]));
    assert_eq!(value("<% %>"), Literal::Octet(Vec::new()));
}

#[test]
fn a_line_comment_is_skipped() {
    // reference/cpp/core/tjs2/tjsLex.cpp:162-186 — a line comment runs to
    // the line feed.
    assert_eq!(
        kinds("a // comment\nb"),
        vec![TokenKind::Symbol, TokenKind::Symbol]
    );
}

#[test]
fn a_block_comment_may_nest() {
    // reference/cpp/core/tjs2/tjsLex.cpp:187-215 — a block comment counts
    // the comments inside it.
    assert_eq!(
        kinds("a /* b /* c */ d */ e"),
        vec![TokenKind::Symbol, TokenKind::Symbol]
    );
}

#[test]
fn a_position_counts_code_units() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1655-1659 — the position of the
    // analyser is the number of code units it has read.
    let units: Vec<u16> = "ab日".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    lexer.next_token().expect("the script lexes");
    assert_eq!(lexer.current_position(), 3);
}

#[test]
fn a_newline_pair_is_one_line_break() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1610-1632 — the carriage return
    // of a newline pair becomes a marker, so the pair is one line break and
    // the positions keep counting both code units.
    let units: Vec<u16> = "a\r\nb".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    lexer.next_token().expect("the script lexes");
    assert_eq!(lexer.current_position(), 2);
    lexer.next_token().expect("the script lexes");
    assert_eq!(lexer.current_position(), 4);
}

#[test]
fn a_conditional_compilation_statement_hides_its_text() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1123-1180 — an `@if` statement
    // keeps the text that follows when its expression is true, and the
    // `@endif` closes it.
    let mut values = PpValues::new();
    values.set("kept", 1);
    let units: Vec<u16> = "@if(kept) a @endif b".encode_utf16().collect();
    let mut lexer = Lexer::new(&units, &mut values);
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token().expect("the script lexes") {
        tokens.push(token);
    }
    assert_eq!(
        tokens.iter().map(|token| token.kind).collect::<Vec<_>>(),
        vec![TokenKind::Symbol, TokenKind::Symbol]
    );
    assert_eq!(text(tokens[0].value.as_ref().unwrap()), "a");
    assert_eq!(text(tokens[1].value.as_ref().unwrap()), "b");
}

#[test]
fn a_conditional_compilation_statement_hides_the_text_it_skips() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1042-1121 — the text between an
    // `@if` and its `@endif` is skipped when the expression is false.
    let mut values = PpValues::new();
    let units: Vec<u16> = "@if(missing) a @endif b".encode_utf16().collect();
    let mut lexer = Lexer::new(&units, &mut values);
    let mut tokens = Vec::new();
    while let Some(token) = lexer.next_token().expect("the script lexes") {
        tokens.push(token);
    }
    assert_eq!(tokens.len(), 1);
    assert_eq!(text(tokens[0].value.as_ref().unwrap()), "b");
}

#[test]
fn an_embeddable_expression_is_read_as_code() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1435-1460 — a string literal that
    // starts with `@` holds code, and the code is read as tokens.
    let units: Vec<u16> = "@\"a&b;c\"".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    let mut kinds = Vec::new();
    while let Some(token) = lexer.next_token().expect("the script lexes") {
        kinds.push(token.kind);
    }
    assert_eq!(
        kinds,
        vec![
            TokenKind::LeftParenthesis,
            TokenKind::ConstVal,
            TokenKind::Plus,
            TokenKind::String,
            TokenKind::LeftParenthesis,
            TokenKind::Symbol,
            TokenKind::RightParenthesis,
            TokenKind::Plus,
            TokenKind::ConstVal,
            TokenKind::RightParenthesis,
        ]
    );
}

#[test]
fn a_block_comment_that_is_not_terminated_is_an_error() {
    // reference/cpp/core/tjs2/tjsLex.cpp:196-211 — a block comment that
    // is not terminated is an error.
    let units: Vec<u16> = "/* abc".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    let error = lexer.next_token().expect_err("the script does not lex");
    assert!(matches!(
        error,
        vm::lexer::LexError::UnterminatedComment { .. }
    ));
}

#[test]
fn a_character_that_begins_no_token_is_an_error() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1496-1501 — a character that
    // cannot begin a token is an error.
    let units: Vec<u16> = "`".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    let error = lexer.next_token().expect_err("the script does not lex");
    assert!(matches!(
        error,
        vm::lexer::LexError::InvalidCharacter { .. }
    ));
}

#[test]
fn a_number_that_cannot_be_read_is_an_error() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1458-1466 — a number that cannot
    // be parsed is an error.
    let units: Vec<u16> = "0x".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    let error = lexer.next_token().expect_err("the script does not lex");
    assert!(matches!(error, vm::lexer::LexError::Number { .. }));
}

#[test]
fn a_string_that_is_not_terminated_is_an_error() {
    // reference/cpp/core/tjs2/tjsLex.cpp:355-364 — a literal that is not
    // terminated is an error.
    let units: Vec<u16> = "\"abc".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::new(&units, &mut values);
    let error = lexer.next_token().expect_err("the script does not lex");
    assert!(matches!(
        error,
        vm::lexer::LexError::UnterminatedLiteral { .. }
    ));
}

#[test]
fn an_expression_reads_its_value() {
    // reference/cpp/core/tjs2/tjsLex.cpp:1661-1680 — an expression that
    // wants its value starts with the token that returns it.
    let units: Vec<u16> = "1+2".encode_utf16().collect();
    let mut values = PpValues::new();
    let mut lexer = Lexer::expression(&units, true, &mut values);
    let mut kinds = Vec::new();
    while let Some(token) = lexer.next_token().expect("the script lexes") {
        kinds.push(token.kind);
    }
    assert_eq!(
        kinds,
        vec![
            TokenKind::Return,
            TokenKind::ConstVal,
            TokenKind::Plus,
            TokenKind::ConstVal,
            TokenKind::Semicolon,
        ]
    );
}

#[test]
fn a_string_is_a_sequence_of_code_units() {
    // reference/cpp/core/tjs2/tjsLex.cpp:254-366 — a string literal is a
    // sequence of code units, and its length is the number of them.
    let string = ScriptString::from_units(vec![0x0041, 0xd800]);
    assert_eq!(string.len(), 2);
    assert_eq!(string.units(), &[0x0041, 0xd800]);
}
