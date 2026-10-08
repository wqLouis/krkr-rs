//! Integration tests for the pure `.ks` parser, the `KAGParser` pull state
//! machine, and the native class.
//!
//! The parser and state machine live in a private module of the `compat`
//! crate (`natives::kag`), so they are compiled directly into this test crate
//! through `#[path]`. That is why the parser/state sources use only relative
//! (`super::…`) imports: the same files are valid both inside the library and
//! here. The native surface is exercised end-to-end through the TJS VM.

mod common;

#[path = "../src/natives/kag/kvp.rs"]
mod kvp;
#[path = "../src/natives/kag/parser.rs"]
mod parser;
#[path = "../src/natives/kag/state.rs"]
mod state;

use state::{Environ, EvalResult, KagParserState};
mod parser_tests {

    use super::parser::*;

    fn events(source: &str) -> Vec<Event> {
        parse(source).expect("parse should succeed").events
    }

    fn tag(name: &str, params: &[(&str, &str)], bracket: Bracket, line: usize) -> Event {
        Event::Tag {
            name: name.to_owned(),
            params: params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            macro_entity: None,
            bracket,
            line,
        }
    }

    fn text(s: &str, line: usize) -> Event {
        Event::Text {
            text: s.to_owned(),
            line,
        }
    }

    // --- file / line handling --------------------------------------------

    #[test]
    fn empty_file_is_an_error() {
        assert_eq!(parse(""), Err(Error::EmptyScenario));
    }

    #[test]
    fn newline_only_input_is_valid_but_empty() {
        assert_eq!(parse("\n").unwrap().events, Vec::new());
        assert_eq!(parse("\r\n").unwrap().events, Vec::new());
    }

    #[test]
    fn crlf_and_lf_parse_identically() {
        assert_eq!(events("one\ntwo\nthree"), events("one\r\ntwo\r\nthree"));
    }

    #[test]
    fn mixed_line_endings() {
        let ev = events("a\r\nb\nc\r");
        assert_eq!(ev, vec![text("a", 1), text("b", 2), text("c", 3)]);
    }

    #[test]
    fn no_trailing_newline_is_handled() {
        assert_eq!(events("a\nb"), vec![text("a", 1), text("b", 2)]);
    }

    #[test]
    fn empty_lines_produce_no_events() {
        assert_eq!(events("a\n\nb"), vec![text("a", 1), text("b", 3)]);
    }

    #[test]
    fn leading_tabs_are_stripped() {
        assert_eq!(events("\t\ttext"), vec![text("text", 1)]);
        let ev = events("\t*label");
        assert_eq!(
            ev,
            vec![Event::Label {
                name: "*label".into(),
                macro_name: None,
                line: 1,
            }]
        );
    }

    #[test]
    fn mid_line_tabs_are_dropped_like_the_reference() {
        assert_eq!(events("a\tb"), vec![text("ab", 1)]);
    }

    #[test]
    fn whitespace_only_line_is_text() {
        assert_eq!(events("  "), vec![text("  ", 1)]);
    }

    // --- labels -----------------------------------------------------------

    #[test]
    fn labels_with_and_without_macro() {
        let scenario = parse("*start\n*end|macroname\n").unwrap();
        assert_eq!(
            scenario.events,
            vec![
                Event::Label {
                    name: "*start".into(),
                    macro_name: None,
                    line: 1,
                },
                Event::Label {
                    name: "*end".into(),
                    macro_name: Some("macroname".into()),
                    line: 2,
                },
            ]
        );
        assert_eq!(scenario.labels.get("*start"), Some(&0));
        assert_eq!(scenario.labels.get("*end"), Some(&1));
    }

    #[test]
    fn label_index_points_at_the_label_event() {
        let scenario = parse("*start\ntext\n@wait\n").unwrap();
        assert_eq!(scenario.events.len(), 3);
        assert_eq!(scenario.labels.get("*start"), Some(&0));
        match &scenario.events[0] {
            Event::Label { name, .. } => assert_eq!(name, "*start"),
            _ => panic!("expected a label event"),
        }
    }

    #[test]
    fn label_with_empty_page_name() {
        let ev = events("*start|\n");
        assert_eq!(
            ev,
            vec![Event::Label {
                name: "*start".into(),
                macro_name: Some("".into()),
                line: 1,
            }]
        );
    }

    #[test]
    fn duplicate_labels_keep_first_occurrence() {
        let scenario = parse("*a\n*a\n").unwrap();
        assert_eq!(scenario.labels.get("*a"), Some(&0));
        assert_eq!(scenario.events.len(), 2);
    }

    // --- @ vs [] ----------------------------------------------------------

    #[test]
    fn at_and_square_tags_are_distinguished() {
        let ev = events("@wait\n[wait]");
        assert_eq!(
            ev,
            vec![
                tag("wait", &[], Bracket::At, 1),
                tag("wait", &[], Bracket::Square, 2),
            ]
        );
    }

    #[test]
    fn at_tag_spans_the_whole_line() {
        let ev = events("@wait 1000");
        assert_eq!(ev, vec![tag("wait", &[("1000", "true")], Bracket::At, 1)]);
    }

    #[test]
    fn at_is_only_special_at_line_start() {
        assert_eq!(events("a@b"), vec![text("a@b", 1)]);
    }

    #[test]
    fn tag_names_and_attribute_names_are_lowercased() {
        let ev = events("@WAIT X=Hello");
        assert_eq!(ev, vec![tag("wait", &[("x", "Hello")], Bracket::At, 1)]);
    }

    // --- tag parameters ---------------------------------------------------

    #[test]
    fn quoted_and_unquoted_params() {
        let ev = events("[bg storage=\"chapter1/f01.jpg\" effect=0]");
        assert_eq!(
            ev,
            vec![tag(
                "bg",
                &[("storage", "chapter1/f01.jpg"), ("effect", "0")],
                Bracket::Square,
                1
            )]
        );
    }

    #[test]
    fn single_quotes_and_brackets_inside_quotes() {
        let ev = events("[x a='it is' b=\"]\"]");
        assert_eq!(
            ev,
            vec![tag("x", &[("a", "it is"), ("b", "]")], Bracket::Square, 1)]
        );
    }

    #[test]
    fn flag_params_get_value_true() {
        let ev = events("[wait noreset]");
        assert_eq!(
            ev,
            vec![tag("wait", &[("noreset", "true")], Bracket::Square, 1)]
        );
    }

    #[test]
    fn empty_and_blank_values() {
        assert_eq!(
            events("[x a=]"),
            vec![tag("x", &[("a", "")], Bracket::Square, 1)]
        );
        assert_eq!(
            events("[x a= ]"),
            vec![tag("x", &[("a", "")], Bracket::Square, 1)]
        );
    }

    #[test]
    fn attribute_order_is_preserved() {
        let ev = events("[x z=1 a=2 m=3]");
        assert_eq!(
            ev,
            vec![tag(
                "x",
                &[("z", "1"), ("a", "2"), ("m", "3")],
                Bracket::Square,
                1
            )]
        );
    }

    #[test]
    fn backtick_escapes_are_removed_from_values() {
        let ev = events("[x a=abc`def]");
        assert_eq!(ev, vec![tag("x", &[("a", "abcdef")], Bracket::Square, 1)]);

        // a backtick can escape a ']' inside an unquoted value
        let ev = events("[x a=ab`]c]");
        assert_eq!(ev, vec![tag("x", &[("a", "ab]c")], Bracket::Square, 1)]);

        // and inside quoted values
        let ev = events("[x a=\"a`\"b\"]");
        assert_eq!(ev, vec![tag("x", &[("a", "a\"b")], Bracket::Square, 1)]);
    }

    #[test]
    fn entity_and_macro_arg_prefixes_are_preserved() {
        let ev = events("[x a=&expr b=%arg]");
        assert_eq!(
            ev,
            vec![tag(
                "x",
                &[("a", "&expr"), ("b", "%arg")],
                Bracket::Square,
                1
            )]
        );
    }

    #[test]
    fn macro_entity_marker_is_consumed() {
        assert_eq!(
            events("[x * a=1]"),
            vec![Event::Tag {
                name: "x".into(),
                params: vec![("a".into(), "1".into())],
                macro_entity: Some(0),
                bracket: Bracket::Square,
                line: 1,
            }]
        );
        assert_eq!(
            events("[x *]"),
            vec![Event::Tag {
                name: "x".into(),
                params: vec![],
                macro_entity: Some(0),
                bracket: Bracket::Square,
                line: 1,
            }]
        );
    }

    #[test]
    fn macro_entity_marker_records_its_position() {
        // The marker is dropped from `params` but its index (where the
        // caller's macro arguments are spliced) is preserved.
        assert_eq!(
            events("[tag foo=bar * baz]"),
            vec![Event::Tag {
                name: "tag".into(),
                params: vec![("foo".into(), "bar".into()), ("baz".into(), "true".into()),],
                macro_entity: Some(1),
                bracket: Bracket::Square,
                line: 1,
            }]
        );
        assert_eq!(
            events("[tag * foo=bar]"),
            vec![Event::Tag {
                name: "tag".into(),
                params: vec![("foo".into(), "bar".into())],
                macro_entity: Some(0),
                bracket: Bracket::Square,
                line: 1,
            }]
        );
    }

    #[test]
    fn escaped_entity_and_macro_prefixes_keep_their_marker() {
        // A backticked leading `&`/`%` is kept as a marker; the state layer
        // strips it and treats the value literally.
        let ev = events("[x a=`&expr b=`%arg]");
        assert_eq!(
            ev,
            vec![tag(
                "x",
                &[("a", "`&expr"), ("b", "`%arg")],
                Bracket::Square,
                1
            )]
        );
        // A backtick escaping anything else is still fully removed.
        assert_eq!(
            events("[x a=`plain]"),
            vec![tag("x", &[("a", "plain")], Bracket::Square, 1)]
        );
    }

    #[test]
    fn empty_attribute_name_is_accepted() {
        // matches the reference: attrib name is empty, value parsed after '='
        assert_eq!(
            events("[x =v]"),
            vec![tag("x", &[("", "v")], Bracket::Square, 1)]
        );
    }

    #[test]
    fn unclosed_quote_is_tolerated_in_at_mode() {
        let ev = events("@x a=\"abc");
        assert_eq!(ev, vec![tag("x", &[("a", "abc")], Bracket::At, 1)]);
    }

    // --- text and inline tags ---------------------------------------------

    #[test]
    fn inline_tags_are_split_out_of_text() {
        let ev = events("Hello [b]world[/b]!");
        assert_eq!(
            ev,
            vec![
                text("Hello ", 1),
                tag("b", &[], Bracket::Square, 1),
                text("world", 1),
                tag("/b", &[], Bracket::Square, 1),
                text("!", 1),
            ]
        );
    }

    #[test]
    fn tag_at_start_and_end_of_line() {
        assert_eq!(
            events("[b]text"),
            vec![tag("b", &[], Bracket::Square, 1), text("text", 1)]
        );
        assert_eq!(
            events("text[b]"),
            vec![text("text", 1), tag("b", &[], Bracket::Square, 1)]
        );
    }

    #[test]
    fn escaped_square_brackets() {
        // `[[` is an escape for a literal `[`; both brackets are consumed
        // (reference normal-character branch), so `[[b]` is the literal
        // text "[b]" — it does not start a tag.
        assert_eq!(events("a[[b"), vec![text("a[b", 1)]);
        assert_eq!(events("[[b]"), vec![text("[b]", 1)]);
        assert_eq!(events("a[[b]"), vec![text("a[b]", 1)]);
        assert_eq!(events("a]b"), vec![text("a]b", 1)]);
        // a single `[` after the escape still starts a tag
        assert_eq!(
            events("a[[[b]"),
            vec![text("a[", 1), tag("b", &[], Bracket::Square, 1)]
        );
    }

    // --- comments ---------------------------------------------------------

    #[test]
    fn comments() {
        assert_eq!(
            events("; a comment\n;second\n"),
            vec![
                Event::Comment {
                    text: " a comment".into(),
                    line: 1,
                },
                Event::Comment {
                    text: "second".into(),
                    line: 2,
                },
            ]
        );
    }

    #[test]
    fn comment_semantics_match_the_reference() {
        // ';' only comments at the start of a line (after tab stripping);
        // mid-line it is ordinary text, as in SkipCommentOrLabel.
        assert_eq!(events("a ; b"), vec![text("a ; b", 1)]);
        assert_eq!(events("\t;tabbed"), {
            let ev = events("\t;tabbed");
            assert!(matches!(&ev[0], Event::Comment { text, .. } if text == "tabbed"));
            ev
        });
        // a space-prefixed ';' is not a comment (only tabs are stripped)
        assert_eq!(events(" ;spaced"), vec![text(" ;spaced", 1)]);
    }

    // --- directives -------------------------------------------------------

    #[test]
    fn directives() {
        let ev = events("%macro mymacro\n%call *start 10\n%if (x)\n%");
        assert_eq!(
            ev,
            vec![
                Event::Directive {
                    name: "macro".into(),
                    args: vec!["mymacro".into()],
                    raw: "%macro mymacro".into(),
                    line: 1,
                },
                Event::Directive {
                    name: "call".into(),
                    args: vec!["*start".into(), "10".into()],
                    raw: "%call *start 10".into(),
                    line: 2,
                },
                Event::Directive {
                    name: "if".into(),
                    args: vec!["(x)".into()],
                    raw: "%if (x)".into(),
                    line: 3,
                },
                Event::Directive {
                    name: "".into(),
                    args: vec![],
                    raw: "%".into(),
                    line: 4,
                },
            ]
        );
    }

    #[test]
    fn percent_mid_line_is_text() {
        assert_eq!(events("a%b"), vec![text("a%b", 1)]);
    }

    // --- syntax errors ----------------------------------------------------

    #[test]
    fn unterminated_square_tags_are_syntax_errors() {
        // `[` at the very end of the line has no name to scan at all
        assert_eq!(
            parse("["),
            Err(Error::Syntax {
                line: 1,
                message: "unexpected end of line in tag",
            })
        );
        assert_eq!(
            parse("abc["),
            Err(Error::Syntax {
                line: 1,
                message: "unexpected end of line in tag",
            })
        );
        assert_eq!(
            parse("[]"),
            Err(Error::Syntax {
                line: 1,
                message: "empty tag name",
            })
        );
        // a name but no closing ']'
        for src in ["[tag", "[tag x", "[tag x=1", "[tag x\"abc"] {
            assert_eq!(
                parse(src),
                Err(Error::Syntax {
                    line: 1,
                    message: "unterminated '[' tag (missing ']')",
                }),
                "expected syntax error for {src:?}"
            );
        }
    }

    #[test]
    fn bare_at_is_a_syntax_error() {
        assert_eq!(
            parse("@"),
            Err(Error::Syntax {
                line: 1,
                message: "unexpected end of line in tag",
            })
        );
    }

    #[test]
    fn missing_value_is_a_syntax_error() {
        for src in ["[x a=", "@x a=", "[x a= ", "[x a=\t"] {
            assert!(matches!(parse(src), Err(Error::Syntax { line: 1, .. })));
        }
    }

    #[test]
    fn unterminated_quoted_value_is_a_syntax_error_in_square_mode() {
        assert!(matches!(
            parse("[x a=\"abc"),
            Err(Error::Syntax { line: 1, .. })
        ));
    }

    #[test]
    fn unterminated_escape_is_a_syntax_error() {
        assert!(matches!(
            parse("[x a=abc`"),
            Err(Error::Syntax { line: 1, .. })
        ));
        assert!(matches!(
            parse("@x a=abc`"),
            Err(Error::Syntax { line: 1, .. })
        ));
    }

    #[test]
    fn error_reports_the_correct_line() {
        assert!(matches!(parse("ok\n["), Err(Error::Syntax { line: 2, .. })));
    }

    // --- end-to-end -------------------------------------------------------

    #[test]
    fn a_small_scenario_parses_in_order() {
        let scenario = parse(
            "; sample\n\
             *start\n\
             Hello [b]world[/b]!\n\
             @wait 1000\n\
             [bg storage=\"bg01.jpg\"]\n\
             %note extra\n",
        )
        .unwrap();
        assert_eq!(
            scenario.events,
            vec![
                Event::Comment {
                    text: " sample".into(),
                    line: 1,
                },
                Event::Label {
                    name: "*start".into(),
                    macro_name: None,
                    line: 2,
                },
                text("Hello ", 3),
                tag("b", &[], Bracket::Square, 3),
                text("world", 3),
                tag("/b", &[], Bracket::Square, 3),
                text("!", 3),
                tag("wait", &[("1000", "true")], Bracket::At, 4),
                tag("bg", &[("storage", "bg01.jpg")], Bracket::Square, 5),
                Event::Directive {
                    name: "note".into(),
                    args: vec!["extra".into()],
                    raw: "%note extra".into(),
                    line: 6,
                },
            ]
        );
        assert_eq!(scenario.labels.get("*start"), Some(&1));
    }

    // --- KAGParserEx multi-line tags (multiLineTagEnabled) ----------------

    fn opts() -> ParseOptions {
        ParseOptions {
            multiline_tags: true,
        }
    }

    #[test]
    fn multiline_line_command_is_joined() {
        // The KAGParserEx readme example: a line command continued over
        // `;`-prefixed lines.
        let scenario = parse_with_options(
            "@tag hoge=123 fuga=test \\\n;    someoption=true \\\n;    andmore=false\n",
            opts(),
        )
        .unwrap();
        assert_eq!(
            scenario.events,
            vec![tag(
                "tag",
                &[
                    ("hoge", "123"),
                    ("fuga", "test"),
                    ("someoption", "true"),
                    ("andmore", "false"),
                ],
                Bracket::At,
                1,
            )]
        );
    }

    #[test]
    fn multiline_bracket_tag_is_joined() {
        // The `[tag ... ]` form works the same; the first line is the
        // logical line number.
        let scenario =
            parse_with_options("hello\n[tag a=1 \\\n; b=2 \\\n; c=3]\n", opts()).unwrap();
        assert_eq!(
            scenario.events,
            vec![
                text("hello", 1),
                tag(
                    "tag",
                    &[("a", "1"), ("b", "2"), ("c", "3")],
                    Bracket::Square,
                    2,
                ),
            ]
        );
    }

    #[test]
    fn multiline_requires_a_space_before_the_backslash() {
        // `hoge=123\` attaches the backslash to the value; it is not a
        // continuation (reference KAGParserEx readme).
        let scenario = parse_with_options("@tag hoge=123\\\n; next=1\n", opts()).unwrap();
        match &scenario.events[0] {
            Event::Tag { name, params, .. } => {
                assert_eq!(name, "tag");
                assert_eq!(params, &[("hoge".to_string(), "123\\".to_string())]);
            }
            other => panic!("expected tag, got {other:?}"),
        }
    }

    #[test]
    fn multiline_continuation_must_be_followed_by_a_semicolon_line() {
        let err = parse_with_options("@tag a=1 \\\nnext=2\n", opts()).unwrap_err();
        assert_eq!(
            err,
            Error::Syntax {
                line: 1,
                message: "line continuation must be followed by a ';' line",
            }
        );
    }

    #[test]
    fn multiline_continuation_at_eof_is_an_error() {
        let err = parse_with_options("@tag a=1 \\\n", opts()).unwrap_err();
        assert_eq!(
            err,
            Error::Syntax {
                line: 1,
                message: "line continuation at end of file",
            }
        );
    }

    #[test]
    fn multiline_is_off_by_default() {
        // Without the option the `\` is an ordinary attribute and the next
        // line is a comment, so no continuation happens.
        let scenario = parse("@tag a=1 \\\n; b=2\n").unwrap();
        assert_eq!(
            scenario.events,
            vec![
                tag("tag", &[("a", "1"), ("\\", "true")], Bracket::At, 1),
                Event::Comment {
                    text: " b=2".into(),
                    line: 2,
                },
            ]
        );
    }
}

// ---------------------------------------------------------------------------
// state machine
// ---------------------------------------------------------------------------

/// A mock evaluator understanding literal integers (enough for `if`/`elsif`
/// and `exp` conditions) and single-quoted strings (enough for `emb`).
fn eval_literal(exp: &str) -> Result<EvalResult, String> {
    let exp = exp.trim();
    if let Some(rest) = exp.strip_prefix('\'')
        && let Some(s) = rest.strip_suffix('\'')
    {
        return Ok(EvalResult::Str(s.to_string()));
    }
    Ok(EvalResult::Integer(exp.parse::<i64>().unwrap_or(0)))
}

/// Drain `next_tag` into an ordered list.
fn collect(
    state: &mut KagParserState,
    eval: &mut dyn FnMut(&str) -> Result<EvalResult, String>,
) -> Vec<(String, Vec<(String, String)>)> {
    let mut load =
        |_name: &str| -> Result<String, String> { Err("unexpected storage load".to_string()) };
    let mut env = Environ {
        eval,
        load_storage: &mut load,
        fire_label: None,
    };
    let mut tags = Vec::new();
    loop {
        match state.next_tag(&mut env) {
            Ok(Some(tag)) => tags.push(tag),
            Ok(None) => break,
            Err(e) => panic!("next_tag error: {e}"),
        }
    }
    tags
}

fn ch_text(tags: &[(String, Vec<(String, String)>)]) -> String {
    tags.iter()
        .filter(|(name, _)| name == "ch")
        .map(|(_, params)| params[0].1.clone())
        .collect()
}

fn tag_names(tags: &[(String, Vec<(String, String)>)]) -> Vec<String> {
    tags.iter().map(|(name, _)| name.clone()).collect()
}

#[test]
fn walks_text_tags_and_line_ends() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\nHi\n@wait 100\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "Hi");
    assert_eq!(
        tags[0],
        (
            "ch".to_string(),
            vec![("text".to_string(), "H".to_string())]
        )
    );
    assert_eq!(
        tags[2],
        (
            "r".to_string(),
            vec![("eol".to_string(), "true".to_string())]
        )
    );
    assert_eq!(
        tags[3],
        (
            "wait".to_string(),
            vec![("100".to_string(), "true".to_string())]
        )
    );
}

#[test]
fn ignore_cr_suppresses_line_ends() {
    let mut state = KagParserState::default();
    state.set_ignore_cr(true);
    state.load_scenario_text("t.ks", "Hi\n").unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert!(!tag_names(&tags).iter().any(|n| n == "r"));
}

#[test]
fn go_to_label_moves_and_reports_missing() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*a\ntext a\n*b\ntext b\n")
        .unwrap();
    state.go_to_label("*b").unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "text b");

    let err = state.go_to_label("*missing").unwrap_err();
    assert!(err.contains("not found"), "unexpected error: {err}");
    // An empty label is a no-op, matching the reference.
    assert!(state.go_to_label("").is_ok());
}

#[test]
fn if_endif_skips_the_false_branch() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text(
            "t.ks",
            "*start\n@if exp=\"1\"\nA\n@endif\n@if exp=\"0\"\nB\n@endif\nC\n",
        )
        .unwrap();
    let mut eval = |e: &str| eval_literal(e);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "AC");
}

#[test]
fn elsif_and_else_select_one_branch() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text(
            "t.ks",
            "*start\n@if exp=\"0\"\nA\n@elsif exp=\"1\"\nB\n@else\nC\n@endif\n",
        )
        .unwrap();
    let mut eval = |e: &str| eval_literal(e);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "B");
}

#[test]
fn ignored_regions_are_skipped() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\n@ignore exp=\"1\"\nA\n@endignore\nB\n")
        .unwrap();
    let mut eval = |e: &str| eval_literal(e);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "B");
}

#[test]
fn records_and_expands_a_macro() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text(
            "t.ks",
            "*start\n[macro name=\"greet\"]\nHi\n[endmacro]\n[greet]\n",
        )
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "Hi");
    assert!(
        state
            .get_macros_entries()
            .iter()
            .any(|(name, _)| name == "greet")
    );
}

#[test]
fn interrupt_is_returned_once() {
    let mut state = KagParserState::default();
    state.load_scenario_text("t.ks", "abc\n").unwrap();
    state.interrupt();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(tags[0].0, "interrupt");
    assert_eq!(
        tags.iter().filter(|(name, _)| name == "interrupt").count(),
        1
    );
}

#[test]
fn jump_moves_within_the_scenario() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\n@jump target=\"*end\"\nA\n*end\nB\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "B");
}

#[test]
fn call_and_return_resume_after_the_call() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text(
            "t.ks",
            "*start\n@call target=\"*sub\"\nA\n@jump target=\"*done\"\n*sub\nB\n@return\n*done\nC\n",
        )
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "BAC");
    assert_eq!(state.get_call_stack_depth(), 0);
}

#[test]
fn store_and_restore_round_trip() {
    let text = "*start\nA\n@wait 10\nB\n";
    let mut state = KagParserState::default();
    state.load_scenario_text("t.ks", text).unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_name: &str| Ok(text.to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
    };
    let first = state.next_tag(&mut env).unwrap();
    assert_eq!(first.unwrap().0, "ch");
    let saved = state.store();

    let mut restored = KagParserState::default();
    let mut eval2 = |_e: &str| Ok(EvalResult::Void);
    let mut load2 = |_name: &str| Ok(text.to_string());
    let mut env2 = Environ {
        eval: &mut eval2,
        load_storage: &mut load2,
        fire_label: None,
    };
    restored.restore(&saved, &mut env2).unwrap();
    assert_eq!(restored.get_storage_name(), "t.ks");
    assert_eq!(restored.get_cur_label(), "*start");
    assert_eq!(restored.get_cur_line(), state.get_cur_line());
}

#[test]
fn clear_empties_the_scenario_but_keeps_properties() {
    let mut state = KagParserState::default();
    state.load_scenario_text("t.ks", "*start\nA\n").unwrap();
    state.set_ignore_cr(true);
    state.clear();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert!(tags.is_empty());
    assert!(state.get_storage_name().is_empty());
    assert!(state.get_ignore_cr());
}

#[test]
fn emb_splices_an_evaluated_string() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "[emb exp=\"'XY'\"]\n")
        .unwrap();
    let mut eval = |e: &str| eval_literal(e);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "XY");
}

// ---------------------------------------------------------------------------
// native class (through the TJS VM)
// ---------------------------------------------------------------------------

use tjs::{Tjs2Engine, TjsValue};

#[test]
fn native_registers_and_interrupt_returns_a_dictionary() {
    let _lock = common::vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");

    let value = engine
        .eval(
            "(function(){ var p = new KAGParser(); p.interrupt(); var t = p.getNextTag(); \
             return t.tagname; })()",
            "kag_test",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String("interrupt".to_string()));
}

#[test]
fn native_tag_dictionary_exposes_taglist() {
    let _lock = common::vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");

    let value = engine
        .eval(
            "(function(){ var p = new KAGParser(); p.interrupt(); var t = p.getNextTag(); \
             return t.taglist[0]; })()",
            "kag_test",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String("tagname".to_string()));
}

#[test]
fn native_properties_and_void_end() {
    let _lock = common::vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");

    let value = engine
        .eval(
            "(function(){ var p = new KAGParser(); p.ignoreCR = true; p.processSpecialTags = false; \
             return (p.ignoreCR ? 1 : 0) + (p.processSpecialTags ? 10 : 0); })()",
            "kag_test",
        )
        .expect("script");
    assert_eq!(value, TjsValue::Integer(1));

    let value = engine
        .eval(
            "(function(){ var p = new KAGParser(); return p.getNextTag(); })()",
            "kag_test",
        )
        .expect("script");
    assert_eq!(value, TjsValue::Void);
}

#[test]
fn native_go_to_label_error_and_macros_object() {
    let _lock = common::vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");

    let err = engine
        .eval(
            "(function(){ var p = new KAGParser(); p.goToLabel('*missing'); })()",
            "kag_test",
        )
        .expect_err("a missing label must raise");
    assert!(err.to_string().contains("not found"), "unexpected: {err}");

    let value = engine
        .eval(
            "(function(){ var p = new KAGParser(); return p.macros; })()",
            "kag_test",
        )
        .expect("script");
    assert!(matches!(value, TjsValue::Object));

    let value = engine
        .eval(
            "(function(){ var p = new KAGParser(); return p.curLineStr; })()",
            "kag_test",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String(String::new()));
}
