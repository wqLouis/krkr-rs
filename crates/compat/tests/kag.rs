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

    #[test]
    fn multiline_ams_motion_tag_keeps_its_continuation_parameters() {
        // `emotion/はてな.ams:12-14` writes an `@motion` line command whose
        // attributes continue on a `;`-prefixed line; KAGParserEx joins
        // them when `multiLineTagEnabled` is on
        // (`reference/cpp/plugins/KAGParser/readme.txt:14-38`).
        let scenario = parse_with_options(
            "@motion id=MARK accel=2 time=750 \\\n; path=\"0, 0, 0, 255\" area=10\n",
            opts(),
        )
        .unwrap();
        assert_eq!(scenario.events.len(), 1);
        match &scenario.events[0] {
            Event::Tag { name, params, .. } => {
                assert_eq!(name, "motion");
                assert!(
                    params
                        .iter()
                        .any(|(k, v)| k == "path" && v == "0, 0, 0, 255")
                );
                assert!(params.iter().any(|(k, v)| k == "area" && v == "10"));
            }
            other => panic!("expected motion tag, got {other:?}"),
        }
    }

    #[test]
    fn duplicate_labels_are_suffixed() {
        // The reference label cache suffixes the second occurrence `:2` and
        // keeps the first under the bare name
        // (`reference/cpp/core/base/KAGParser.cpp:193-238`).
        let scenario = parse("*foo\n*foo\n*foo\n").unwrap();
        assert_eq!(
            scenario.labels.get("*foo").copied(),
            Some(0),
            "first occurrence keeps the bare name"
        );
        assert_eq!(scenario.labels.get("*foo:2").copied(), Some(1));
        assert_eq!(scenario.labels.get("*foo:3").copied(), Some(2));
        assert!(matches!(
            &scenario.events[1],
            Event::Label { name, .. } if name == "*foo:2"
        ));
    }

    #[test]
    fn empty_label_aliases_the_previous_one() {
        // An empty `*` reuses the previous label name; because that name is
        // already in the cache, the reference then suffixes the alias `:2`
        // (`reference/cpp/core/base/KAGParser.cpp:207-238`).
        let scenario = parse("*foo\n*\n").unwrap();
        assert_eq!(scenario.labels.get("*foo").copied(), Some(0));
        assert_eq!(scenario.labels.get("*foo:2").copied(), Some(1));
        assert!(matches!(
            &scenario.events[1],
            Event::Label { name, .. } if name == "*foo:2"
        ));
    }

    #[test]
    fn empty_first_label_is_an_error() {
        // Without a previous label an empty one cannot be aliased
        // (`reference/cpp/core/base/KAGParser.cpp:213-216`).
        assert_eq!(parse("*\n"), Err(Error::CannotOmitFirstLabelName));
    }

    #[test]
    fn iscript_block_is_collected_verbatim() {
        // The reference collects `[iscript]` … `[endscript]` lines without
        // parsing their `[`/`]` (`reference/cpp/core/base/KAGParser.cpp:1130-1176`).
        let scenario = parse("*start\n[iscript]\nvar a = [1, 2];\n[endscript]\nafter\n").unwrap();
        assert_eq!(
            scenario.events,
            vec![
                Event::Label {
                    name: "*start".into(),
                    macro_name: None,
                    line: 1,
                },
                Event::Script {
                    text: "var a = [1, 2];\r\n".into(),
                    start_line: 2,
                    line: 2,
                },
                text("after", 5),
            ]
        );
    }

    #[test]
    fn iscript_without_endscript_is_an_error() {
        assert_eq!(
            parse("[iscript]\nvar a = 1;\n"),
            Err(Error::InlineScriptNotEnd)
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
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
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
fn cur_line_and_cur_pos_are_zero_based() {
    // `Rewind` sets `CurLine = 0`; after `GoToLabel` the cursor is the
    // label's 0-based line, and a returned character reports the source
    // line and the column just past it
    // (`reference/cpp/core/base/KAGParser.cpp:987-991`, `:1015-1053`,
    // `:1464-1506`).
    let mut state = KagParserState::default();
    state.load_scenario_text("t.ks", "*start\nA\n").unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    state.go_to_label("*start").unwrap();
    assert_eq!(state.cur_line(), 0);

    let (name, _) = state.next_tag(&mut env).unwrap().unwrap();
    assert_eq!(name, "ch");
    assert_eq!(state.cur_line(), 1);
    assert_eq!(state.cur_line_str(), "A");
    assert_eq!(state.cur_pos(), 1);
}

#[test]
fn percent_line_is_walked_as_text() {
    // The base reference has no `%` handling at all, so a `%` line is
    // ordinary text (`reference/cpp/core/base/KAGParser.cpp:1464-1512`).
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "%macro mymacro\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "%macro mymacro");
}

#[test]
fn multi_line_tag_enabled_joins_continuations() {
    // KAGParserEx reads `multiLineTagEnabled` when the scenario is parsed
    // (`reference/cpp/plugins/KAGParser/readme.txt:14-38`); the game sets
    // it before `loadScenario` (`system/animationsequence.tjs:15`).
    let text = "@motion id=MARK time=750 \\\n; path=\"0, 0, 255\"\n";

    let mut off = KagParserState::default();
    off.load_scenario_text("t.ams", text).unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    let (name, params) = off.next_tag(&mut env).unwrap().unwrap();
    assert_eq!(name, "motion");
    assert!(!params.iter().any(|(k, _)| k == "path"));

    let mut on = KagParserState::default();
    on.multi_line_tag_enabled = true;
    on.load_scenario_text("t.ams", text).unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    let (name, params) = on.next_tag(&mut env).unwrap().unwrap();
    assert_eq!(name, "motion");
    assert!(params.iter().any(|(k, v)| k == "path" && v == "0, 0, 255"));
}

#[test]
fn param_macros_expand_into_a_tag() {
    // KAGParserEx replaces an attribute matching a `paramMacros` key with
    // the registered parameter list
    // (`reference/cpp/plugins/KAGParser/readme.txt:51-60`).
    let mut state = KagParserState::default();
    state.set_param_macros(vec![(
        "greet".to_string(),
        vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ],
    )]);
    assert_eq!(state.param_macros_entries().len(), 1);
    state
        .load_scenario_text("t.ks", "*start\n[x greet]\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    let (name, params) = state.next_tag(&mut env).unwrap().unwrap();
    assert_eq!(name, "x");
    assert_eq!(
        params,
        vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]
    );
}

#[test]
fn cur_line_str_uses_the_emb_expansion_buffer() {
    // While an expansion is active the reference points `CurLineStr` at
    // `LineBuffer` and `CurPos` at the buffer column
    // (`reference/cpp/core/base/KAGParser.cpp:1873-1884`).
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "[emb exp=\"'XY'\"]\n")
        .unwrap();
    let mut eval = |e: &str| eval_literal(e);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    let (_, first_params) = loop {
        let (name, params) = state.next_tag(&mut env).unwrap().unwrap();
        if name == "ch" {
            break (name, params);
        }
    };
    assert_eq!(first_params[0].1, "X");
    assert_eq!(state.cur_line_str(), "XY");
    assert_eq!(state.cur_pos(), 1);
}

#[test]
fn on_script_fires_for_an_inline_block() {
    // The walk fires `onScript(script, storageShortName, scriptStart)` as it
    // passes a collected inline script block
    // (`reference/cpp/core/base/KAGParser.cpp:1161-1172`).
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\n[iscript]\nvar x = 1;\n[endscript]\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut script = String::new();
    let mut storage = String::new();
    let mut start = usize::MAX;
    let mut fire_script = |s: &str, st: &str, line: usize| {
        script = s.to_string();
        storage = st.to_string();
        start = line;
    };
    let mut tags = Vec::new();
    {
        let mut env = Environ {
            eval: &mut eval,
            load_storage: &mut load,
            fire_label: None,
            fire_script: Some(&mut fire_script),
            fire_jump: None,
            fire_call: None,
            fire_return: None,
            fire_scenario_load: None,
            fire_scenario_loaded: None,
        };
        loop {
            match state.next_tag(&mut env) {
                Ok(Some(tag)) => tags.push(tag),
                Ok(None) => break,
                Err(e) => panic!("next_tag error: {e}"),
            }
        }
    }
    assert!(tags.is_empty());
    assert_eq!(script, "var x = 1;\r\n");
    assert_eq!(storage, "t.ks");
    assert_eq!(start, 2);
}

#[test]
fn jump_veto_skips_the_jump() {
    // A false `onJump` result vetoes the jump, so the walk falls through
    // (`reference/cpp/core/base/KAGParser.cpp:1897-1918`).
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\n@jump target=\"*end\"\nA\n*end\nB\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut fire_jump = |name: &str, params: &[(String, String)]| -> bool {
        assert_eq!(name, "jump");
        assert!(params.iter().any(|(k, v)| k == "target" && v == "*end"));
        false
    };
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: Some(&mut fire_jump),
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    let mut tags = Vec::new();
    loop {
        match state.next_tag(&mut env) {
            Ok(Some(tag)) => tags.push(tag),
            Ok(None) => break,
            Err(e) => panic!("next_tag error: {e}"),
        }
    }
    assert_eq!(ch_text(&tags), "AB");
}

#[test]
fn assign_preserves_process_special_tags_and_interrupted() {
    // `operator=` copies `DebugLevel`/`IgnoreCR` but not
    // `ProcessSpecialTags`/`Interrupted` (`KAGParser.cpp:395-466`).
    let mut src = KagParserState::default();
    src.load_scenario_text("t.ks", "*start\nA\n").unwrap();
    src.process_special_tags = false;
    src.interrupt();
    src.debug_level = 2;
    let mut dst = KagParserState::default();
    dst.assign_from(&src);
    assert!(dst.process_special_tags);
    assert!(!dst.interrupted);
    assert_eq!(dst.debug_level, 2);
}

#[test]
fn ignore_cr_suppresses_line_ends() {
    let mut state = KagParserState::default();
    state.ignore_cr = true;
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
            .macros_entries()
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
    assert_eq!(state.call_stack_depth(), 0);
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
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
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
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    restored.restore(&saved, &mut env2).unwrap();
    assert_eq!(restored.storage_name(), "t.ks");
    assert_eq!(restored.cur_label(), "*start");
    assert_eq!(restored.cur_line(), state.cur_line());
}

#[test]
fn store_carries_the_active_line_buffer() {
    // `Store` writes the current `lineBuffer`/`lineBufferUsing`
    // (`reference/cpp/core/base/KAGParser.cpp:591-597`).
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "[emb exp=\"'XY'\"]\n")
        .unwrap();
    let mut eval = |e: &str| eval_literal(e);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    loop {
        let (name, _) = state.next_tag(&mut env).unwrap().unwrap();
        if name == "ch" {
            break;
        }
    }
    assert!(state.line_buffer_using());
    assert_eq!(state.line_buffer(), "XY");
    let saved = state.store();
    assert!(saved.contains("lineBuffer=XY"), "{saved}");
    assert!(saved.contains("lineBufferUsing=1"), "{saved}");
}

#[test]
fn call_stack_snapshot_carries_the_active_line_buffer() {
    // Each call-stack entry keeps the `LineBuffer`/`LineBufferUsing` that
    // were live at the call site (`reference/cpp/core/base/KAGParser.cpp:591-597`,
    // `595-634`).
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\n[emb exp=\"'XY'\"]\n*sub\nB\n")
        .unwrap();
    let mut eval = |e: &str| eval_literal(e);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };
    // Walk into the expansion so the splice buffer "XY" is active, then
    // enter a call: the snapshot must capture the live buffer.
    loop {
        let (name, _) = state.next_tag(&mut env).unwrap().unwrap();
        if name == "ch" {
            break;
        }
    }
    assert!(state.line_buffer_using());
    state.call_label("*sub", &mut env).unwrap();
    let snapshots = state.call_stack_snapshots();
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].line_buffer_using);
    assert_eq!(snapshots[0].line_buffer, "XY");
}

#[test]
fn set_macros_registers_an_expandable_macro() {
    // `set_macros` replaces the live macro table the walk consults when it
    // meets `[name]` (`reference/cpp/core/base/KAGParser.cpp:2471-2481`).
    let mut state = KagParserState::default();
    state.set_macros(vec![(
        "greet".to_string(),
        "[ch text=\"Z\"][macropop]".to_string(),
    )]);
    assert_eq!(
        state.macros_entries(),
        vec![("greet".to_string(), "[ch text=\"Z\"][macropop]".to_string())]
    );
    state
        .load_scenario_text("t.ks", "*start\n[greet]\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert_eq!(ch_text(&tags), "Z");
}

#[test]
fn clear_empties_the_scenario_but_keeps_properties() {
    let mut state = KagParserState::default();
    state.load_scenario_text("t.ks", "*start\nA\n").unwrap();
    state.ignore_cr = true;
    state.clear();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let tags = collect(&mut state, &mut eval);
    assert!(tags.is_empty());
    assert!(state.storage_name().is_empty());
    assert!(state.ignore_cr);
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

#[test]
fn state_accessors_and_call_label() {
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\nA\n*sub\nB\n")
        .unwrap();
    // A real-valued evaluator also exercises `EvalResult::Real`.
    let mut eval = |_e: &str| Ok(EvalResult::Real(1.0));
    let mut load = |_name: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };

    // `CallLabel`: push the call stack and jump to the label.
    state.call_label("*sub", &mut env).unwrap();
    assert_eq!(state.call_stack_depth(), 1);

    // Accessors exposed to the native layer.
    let _ = state.cur_pos();
    assert!(!state.cur_line_str().is_empty());
    state.process_special_tags = false;
    assert!(!state.process_special_tags);
    state.process_special_tags = true;
    assert!(state.process_special_tags);

    // A fresh state has no macro-argument level to pop.
    assert!(state.pop_macro_args_public().is_err());

    state.clear_call_stack();
    assert_eq!(state.call_stack_depth(), 0);

    // Remaining native-facing accessors.
    state.debug_level = 2;
    assert_eq!(state.debug_level, 2);
    assert!(state.macros_entries().is_empty());
    assert!(state.macro_params_entries().is_none());
    state.interrupt();
    state.reset_interrupt();
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

// ---------------------------------------------------------------------------
// reference surface: store / restore / assign / property semantics
// (reference/cpp/core/base/KAGParser.cpp:2213-2560)
// ---------------------------------------------------------------------------

/// Evaluate `script` on a fresh bound VM (the native tests each build their
/// own engine under the shared `vm_lock`).
fn eval_native(script: &str) -> Result<TjsValue, String> {
    let _lock = common::vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let mut host = compat::CompatHost::new();
    host.install(&engine).expect("install compat surface");
    engine.eval(script, "kag_test").map_err(|e| e.to_string())
}

#[test]
fn native_store_returns_the_reference_dictionary_shape() {
    // `Store()` returns a dictionary with the reference top-level members
    // plus the nested `macros`, `macroArgs` and `callStack` containers
    // (KAGParser.cpp:469-636).
    let value = eval_native(
        "(function(){ var p = new KAGParser(); var d = p.store(); \
         return typeof d.macros + ',' + typeof d.macroArgs + ',' + typeof d.callStack \
         + ',' + typeof d.data + ',' + d.curLine + ',' + d.ExcludeLevel \
         + ',' + d.storageName.length + ',' + d.macroArgs.count + ',' + d.callStack.count; })()",
    )
    .expect("script");
    assert_eq!(
        value,
        TjsValue::String("Object,Object,Object,String,0,-1,0,0,0".to_string())
    );
}

#[test]
fn native_store_restore_round_trips_state() {
    let value = eval_native(
        "(function(){ var p = new KAGParser(); p.debugLevel = 3; \
         var d = p.store(); var q = new KAGParser(); q.restore(d); \
         return q.debugLevel; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::Integer(3));
}

#[test]
fn native_restore_imports_a_reference_format_dictionary() {
    // The reference `Restore` member names (KAGParser.cpp:723-906): scalars,
    // the nested `macros` dictionary and the `macroArgs` array (with its
    // `count` and the matching `macroArgStackDepth` check at 768-769).
    let value = eval_native(
        "(function(){ var p = new KAGParser(); \
         var d = %[ curLine: 4, ExcludeLevel: 2, macroArgStackDepth: 1, \
                    macros: %[ greet: 'Hello' ], \
                    macroArgs: [%[ a: '1' ]], \
                    callStack: [] ]; \
         p.restore(d); \
         return p.curLine + ',' + p.macros['greet'] + ',' + p.mp['a'] \
         + ',' + p.store().ExcludeLevel; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::String("4,Hello,1,2".to_string()));

    // `TVPKAGMalformedSaveData` when the depth does not match the array
    // (KAGParser.cpp:51, 768-769).
    let error = eval_native(
        "(function(){ var p = new KAGParser(); \
         p.restore(%[ macroArgs: [%[ a: '1' ]] ]); })()",
    )
    .expect_err("a mismatched macroArgStackDepth must raise");
    assert!(error.contains("Malformed savedata"), "unexpected: {error}");
}

#[test]
fn native_assign_copies_a_parser_and_rejects_non_parsers() {
    let value = eval_native(
        "(function(){ var a = new KAGParser(); a.debugLevel = 2; \
         var b = new KAGParser(); b.assign(a); return b.debugLevel; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::Integer(2));

    // `TVPKAGSpecifyKAGParser` for a non-parser argument
    // (KAGParser.cpp:59-60, 2281-2287).
    for expression in [
        "(function(){ var p = new KAGParser(); p.assign(5); })()",
        "(function(){ var p = new KAGParser(); p.assign(%[]); })()",
        "(function(){ var p = new KAGParser(); p.assign(); })()",
    ] {
        let error = eval_native(expression).expect_err(&format!("{expression:?} must raise"));
        assert!(
            error.contains("Please specify KAGParser object") || error.contains("requires 1"),
            "unexpected: {error}"
        );
    }
}

#[test]
fn native_macros_property_is_read_only_and_stable() {
    // `TJS_DENY_NATIVE_PROP_SETTER` (KAGParser.cpp:2482).
    let error = eval_native("(function(){ var p = new KAGParser(); p.macros = %[]; })()")
        .expect_err("the macros property must be read-only");
    assert!(!error.is_empty());

    // `GetMacrosNoAddRef` hands the same dictionary out on every read
    // (KAGParser.cpp:2471-2481) — identity is stable while unchanged.
    let value = eval_native(
        "(function(){ var p = new KAGParser(); return p.macros === p.macros ? 1 : 0; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::Integer(1));

    // `mp` is `null` on an empty macro-argument stack (the null
    // `tTJSVariant(params, params)` at KAGParser.cpp:2499-2501, fed by
    // `GetMacroTopNoAddRef` returning nullptr at 2191-2196).
    let value =
        eval_native("(function(){ var p = new KAGParser(); return p.mp === null ? 1 : 0; })()")
            .expect("script");
    assert_eq!(value, TjsValue::Integer(1));
}

#[test]
fn native_debug_level_uses_the_reference_integer_conversion() {
    // `SetDebugLevel((tTVPKAGDebugLevel)(tjs_int)(*param))`
    // (KAGParser.cpp:2464): reals truncate, objects raise.
    let value = eval_native(
        "(function(){ var p = new KAGParser(); p.debugLevel = 2.7; return p.debugLevel; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::Integer(2));

    let error = eval_native("(function(){ var p = new KAGParser(); p.debugLevel = %[]; })()")
        .expect_err("an object must not silently become a debug level");
    assert!(!error.is_empty());
}

#[test]
fn native_multi_line_tag_enabled_property_round_trips() {
    // KAGParserEx adds the `multiLineTagEnabled` property
    // (`reference/cpp/plugins/KAGParser/readme.txt:14-38`); the game sets
    // it before `loadScenario` (`system/animationsequence.tjs:15`).
    let value = eval_native(
        "(function(){ var p = new KAGParser(); p.multiLineTagEnabled = true; \
         return p.multiLineTagEnabled ? 1 : 0; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::Integer(1));

    let value = eval_native(
        "(function(){ var p = new KAGParser(); return p.multiLineTagEnabled ? 1 : 0; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::Integer(0));
}

#[test]
fn native_multi_line_tag_enabled_joins_ams_continuations() {
    // `system/animationsequence.tjs:15` enables multi-line tags before
    // loading the `.ams`; the joined `@motion` keeps the `;`-line
    // parameters (`emotion/はてな.ams:12-14`,
    // `reference/cpp/plugins/KAGParser/readme.txt:14-38`).
    let _lock = common::vm_lock();
    let env = common::Env::with_storage();
    std::fs::write(
        env.game_dir().expect("mounted game dir").join("motion.ams"),
        b"*loop\n@motion id=MARK accel=2 time=750 \\\n; path=\"0, 0, 255\" area=10 zoom=1 rotate=0\n",
    )
    .expect("write motion.ams");

    let value = env
        .eval(
            "(function(){ var p = new KAGParser(); p.multiLineTagEnabled = true; \
             p.loadScenario('motion.ams'); var out = ''; while (true) { \
             var t = p.getNextTag(); if (!t) break; \
             if (t.tagname == 'motion') out = t.path + '|' + t.area + '|' + t.zoom + '|' + t.rotate; } \
             return out; })()",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String("0, 0, 255|10|1|0".to_string()));
}

#[test]
fn native_cur_line_is_zero_based() {
    // `Rewind` sets `CurLine = 0` and `CurLineStr` is the current source
    // line (`reference/cpp/core/base/KAGParser.cpp:987-991`); the character
    // on the second source line therefore reports `curLine == 1`.
    let _lock = common::vm_lock();
    let env = common::Env::with_storage();
    std::fs::write(
        env.game_dir().expect("mounted game dir").join("line.ks"),
        b"*start\nA\n",
    )
    .expect("write line.ks");

    let value = env
        .eval(
            "(function(){ var p = new KAGParser(); p.loadScenario('line.ks'); \
             while (true) { var t = p.getNextTag(); if (!t) break; \
             if (t.tagname == 'ch') return p.curLine + '|' + p.curLineStr; } \
             return 'none'; })()",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String("1|A".to_string()));
}

#[test]
fn native_param_macros_property_round_trips() {
    // KAGParserEx exposes a writable `paramMacros` dictionary whose values
    // are `[name, value, name, value, ...]` arrays
    // (`reference/cpp/plugins/KAGParser/readme.txt:51-60`).
    let value = eval_native(
        "(function(){ var p = new KAGParser(); \
         p.paramMacros = %[ greet: [ 'a', '1', 'b', '2' ] ]; \
         var d = p.paramMacros; \
         return d['greet'][0] + d['greet'][1] + d['greet'][2] + d['greet'][3]; })()",
    )
    .expect("script");
    assert_eq!(value, TjsValue::String("a1b2".to_string()));
}

#[test]
fn native_param_macros_expands_into_a_tag() {
    // A tag attribute matching a `paramMacros` key is replaced by the
    // registered parameter list at run time
    // (`reference/cpp/plugins/KAGParser/readme.txt:51-60`).
    let _lock = common::vm_lock();
    let env = common::Env::with_storage();
    std::fs::write(
        env.game_dir().expect("mounted game dir").join("pm.ks"),
        b"*start\n[x greet]\n",
    )
    .expect("write pm.ks");

    let value = env
        .eval(
            "(function(){ var p = new KAGParser(); \
             p.paramMacros = %[ greet: [ 'a', '1', 'b', '2' ] ]; \
             p.loadScenario('pm.ks'); var out = ''; while (true) { \
             var t = p.getNextTag(); if (!t) break; \
             if (t.tagname == 'x') out = t.a + t.b; } return out; })()",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String("12".to_string()));
}

#[test]
fn native_macros_table_is_live() {
    // `GetMacrosNoAddRef` hands out the one shared dictionary; a script
    // mutation must be seen by the walk
    // (`reference/cpp/core/base/KAGParser.cpp:2471-2481`).
    let _lock = common::vm_lock();
    let env = common::Env::with_storage();
    std::fs::write(
        env.game_dir().expect("mounted game dir").join("m.ks"),
        b"*start\n[greet]\n",
    )
    .expect("write m.ks");
    let value = env
        .eval(
            "(function(){ var p = new KAGParser(); \
             p.macros['greet'] = '[ch text=\"Z\"]'; \
             p.loadScenario('m.ks'); var out = ''; while (true) { \
             var t = p.getNextTag(); if (!t) break; \
             if (t.tagname == 'ch') out += t.text; } return out; })()",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String("Z".to_string()));
}

#[test]
fn native_on_script_fires() {
    // `onScript(script, storageShortName, scriptStart)` fires as the walk
    // passes an `[iscript]` block (`KAGParser.cpp:1161-1172`).
    let _lock = common::vm_lock();
    let env = common::Env::with_storage();
    std::fs::write(
        env.game_dir().expect("mounted game dir").join("s.ks"),
        b"*start\n[iscript]\nvar x = 1;\n[endscript]\n",
    )
    .expect("write s.ks");
    env.run(
        "class KagScriptProbe extends KAGParser {\n\
         function KagScriptProbe() { KAGParser(); }\n\
         function onScript(script, storage, line) {\n\
           global.probeScript = script; global.probeLine = line;\n\
         }\n\
         }",
    );
    let value = env
        .eval(
            "(function(){ var p = new KagScriptProbe(); p.loadScenario('s.ks'); \
             while (true) { var t = p.getNextTag(); if (!t) break; } \
             return global.probeScript + '|' + global.probeLine; })()",
        )
        .expect("script");
    assert_eq!(value, TjsValue::String("var x = 1;\r\n|2".to_string()));
}

#[test]
fn state_reference_store_accessors_round_trip() {
    // The native `store()` dictionary is fed from these accessors
    // (`natives/kag/mod.rs:build_store_dict`); pin them at the state level.
    let mut state = KagParserState::default();
    state
        .load_scenario_text("t.ks", "*start\nA\n*sub\nB\n")
        .unwrap();
    let mut eval = |_e: &str| Ok(EvalResult::Void);
    let mut load = |_n: &str| Err("unexpected storage load".to_string());
    let mut env = Environ {
        eval: &mut eval,
        load_storage: &mut load,
        fire_label: None,
        fire_script: None,
        fire_jump: None,
        fire_call: None,
        fire_return: None,
        fire_scenario_load: None,
        fire_scenario_loaded: None,
    };

    assert_eq!(state.storage_short_name(), state.storage_name());
    assert_eq!(state.exclude_level(), -1);
    assert!(state.exclude_level_stack().is_empty());
    assert!(state.if_level_executed_stack().is_empty());
    assert_eq!(state.macro_args_depth(), 0);
    assert!(state.macro_args_entries().is_empty());

    // A pushed call-stack entry maps onto the reference `tCallStackData`
    // fields (KAGParser.cpp:546-634).
    state.call_label("*sub", &mut env).unwrap();
    let snapshots = state.call_stack_snapshots();
    assert_eq!(snapshots.len(), 1);
    // The entry maps onto the reference `tCallStackData` fields
    // (KAGParser.cpp:595-634): the return storage, the stacks (encoded like
    // `StoreIntStackToDic`) and the macro-argument base.
    assert_eq!(snapshots[0].storage, state.storage_name());
    assert_eq!(
        snapshots[0].exclude_level_stack,
        state::encode_int_stack(state.exclude_level_stack())
    );
    assert_eq!(
        snapshots[0].if_level_executed_stack,
        state::encode_bool_stack(state.if_level_executed_stack())
    );
    assert_eq!(snapshots[0].macro_arg_stack_base, state.macro_args_base());
    assert_eq!(snapshots[0].macro_arg_stack_depth, state.macro_args_depth());
    assert_eq!(snapshots[0].exclude_level, state.exclude_level());
    assert_eq!(snapshots[0].if_level, state.if_level());
    let _ = state.if_level();
    let _ = state.macro_args_base();
    let _ = (
        &snapshots[0].label,
        snapshots[0].offset,
        &snapshots[0].org_line_str,
        snapshots[0].pos,
    );

    // The stack encodings match the reference `StoreIntStackToDic` /
    // `StoreBoolStackToDic` formats (KAGParser.cpp:637-659, 704-722).
    assert_eq!(state::encode_int_stack(&[1, 256]), "0000000100000100");
    assert_eq!(state::encode_bool_stack(&[true, false, true]), "101");
}

#[test]
fn native_object_valued_exp_is_truthy() {
    let _lock = common::vm_lock();
    let env = common::Env::with_storage();
    std::fs::write(
        env.game_dir()
            .expect("mounted game dir")
            .join("scenario.ks"),
        b"*start\n@if exp=\"[]\"\nA\n@endif\n",
    )
    .expect("write scenario.ks");

    let value = env
        .eval(
            "(function(){ var p = new KAGParser(); p.loadScenario('scenario.ks'); \
             var out = ''; while (true) { var t = p.getNextTag(); if (!t) break; \
             if (t.tagname == 'ch') out += t.text; } return out; })()",
        )
        .expect("script");
    // An object expression is truthy (`operator bool` is `Object != nullptr`),
    // so the `@if` branch runs
    // (`reference/cpp/core/base/KAGParser.cpp:2168-2171`).
    assert_eq!(value, TjsValue::String("A".to_string()));
}

#[test]
fn native_object_valued_exp_is_evaluated_once() {
    let _lock = common::vm_lock();
    let env = common::Env::with_storage();
    // The array literal has a side effect (it increments the global counter);
    // a single evaluation must leave the counter at 1.
    std::fs::write(
        env.game_dir()
            .expect("mounted game dir")
            .join("scenario.ks"),
        b"*start\n@if exp=\"[global.kagEvalCount = global.kagEvalCount + 1]\"\nA\n@endif\n",
    )
    .expect("write scenario.ks");

    let value = env
        .eval(
            "(function(){ global.kagEvalCount = 0; var p = new KAGParser(); \
             p.loadScenario('scenario.ks'); var out = ''; while (true) { \
             var t = p.getNextTag(); if (!t) break; \
             if (t.tagname == 'ch') out += t.text; } \
             return out + '|' + global.kagEvalCount; })()",
        )
        .expect("script");
    // Deriving the object's string form must not re-evaluate the expression
    // that produced it (`reference/cpp/core/base/KAGParser.cpp:2138-2141`).
    assert_eq!(value, TjsValue::String("A|1".to_string()));
}

#[test]
fn eval_result_object_carries_truthiness_and_string_form() {
    // `operator bool` is `Object != nullptr` and the string path uses
    // `ToString` (`reference/cpp/core/base/KAGParser.cpp:2138-2141`,
    // `:2168-2171`).
    let truthy = EvalResult::Object {
        text: "abc".to_owned(),
        truthy: true,
    };
    assert!(truthy.truthy());
    assert_eq!(truthy.to_string_repr().as_deref(), Some("abc"));

    // TJS `null` is an object with `truthy == false`.
    let null = EvalResult::Object {
        text: "null".to_owned(),
        truthy: false,
    };
    assert!(!null.truthy());
}
