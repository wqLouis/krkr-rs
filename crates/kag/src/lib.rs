//! KiriKiri **KAG** scenario (`.ks`) parser.
//!
//! This crate re-implements the scenario-loading and tag-splitting half of
//! the reference `tTJSNI_KAGParser` (`reference/cpp/core/base/KAGParser.cpp`)
//! as a pure-Rust library. It depends on nothing but `std` (+ `thiserror`):
//! no TJS2 VM, no XP3 archive reader, no other workspace crate.
//!
//! The input is already-decoded UTF-8 text (the CP932 → UTF-8 conversion the
//! reference does in `CharacterSet.cpp` is out of scope and handled
//! elsewhere). The output is an ordered list of [`Event`]s plus a label
//! index ([`Scenario`]).
//!
//! # Line model
//!
//! The reference first splits the scenario into lines on `\r`, `\n` and
//! `\r\n`, and strips leading tabs from every line (`LoadScenario` pass 2).
//! An empty scenario (no lines at all) is an error. Each line is then
//! classified by its first character:
//!
//! | first char | meaning |
//! |---|---|
//! | `;` | comment line → [`Event::Comment`] (the reference *skips* it) |
//! | `*` | label → [`Event::Label`], `*name` or `*name|macro` |
//! | `@` | line-command tag → [`Event::Tag`] with [`Bracket::At`] |
//! | `%` | directive → [`Event::Directive`] |
//! | `[` | square-bracket tag → [`Event::Tag`] with [`Bracket::Square`] |
//! | anything else | plain text → [`Event::Text`] (inline tags split out) |
//!
//! # Tag syntax (as in the reference `_GetNextTag`)
//!
//! * `@tag ...` is a *line command*: it spans the rest of the line and needs
//!   no closing bracket. `[tag ...]` is *bracket mode*: it ends at the
//!   matching `]`. The two are reported via [`Bracket`].
//! * The tag name and every attribute name are lower-cased, exactly like
//!   `ttstr::ToLowerCase()` in the reference.
//! * Attributes are `name=value`, `name="quoted value"`, `name='quoted'` or
//!   bare `name` (a flag, value `"true"`).
//! * Values may be quoted with `"` or `'`; inside quotes, spaces and `]` are
//!   literal. Unquoted values run to the next whitespace (or `]`).
//! * A backtick `` ` `` escapes the following character anywhere inside a
//!   value (both quoted and unquoted) and is removed from the result.
//! * `&`/`%` value prefixes (TJS expression / macro argument) are preserved
//!   verbatim — this parser does not evaluate them.
//! * A lone `*` where an attribute name is expected is the "macro entity
//!   all" marker; it is consumed and not stored (the reference expands macro
//!   arguments there).
//! * In plain text, `[[` is an escape for a literal `[`; `[` otherwise
//!   starts an inline tag. A stray `[` with no closing `]` is a syntax
//!   error, matching the reference `TVPKAGSyntaxError`.
//! * Mid-line tabs are dropped from text (the reference emits no character
//!   tag for them).
//!
//! # Deliberate divergences from the reference (milestone scope)
//!
//! * The reference is a *pull* parser (`getNextTag`) that returns one
//!   character per "ch" tag, emits `r` tags for line breaks and treats
//!   comment/label lines as pure control flow. This crate is a *push*
//!   parser: one [`Event::Text`] per contiguous text run, one
//!   [`Event::Tag`] per tag, and empty lines produce no event (the
//!   reference emits an `r eol=true` tag).
//! * `[iscript]`/`@iscript` … `[endscript]`/`@endscript` blocks are parsed
//!   as ordinary tags (the reference consumes them and fires `onScript`).
//! * The trailing-`\` and trailing-`[p]` line-continuation rules only affect
//!   `r`-tag emission in the reference; since this crate has no `r` events,
//!   a trailing `\` is kept in the text.
//! * `cond`, `if`/`elsif`/`else`, `emb`, `macro`, `jump`/`call`/`return`
//!   and `%`-directives are all *parsed* but not *executed*: no expression
//!   evaluation, macro expansion or control flow happens here.
//! * Label names keep their leading `*` (the reference's label cache is
//!   keyed by `*name`, and `GoToLabel`/`CallLabel` receive `*`-prefixed
//!   names).
//! * Duplicate labels: the reference suffixes duplicates (`name:2`, …) for
//!   jump disambiguation; this crate records the first occurrence in
//!   [`Scenario::labels`].
//! * Line numbers in events are 1-based (the reference's `curLine` is
//!   0-based).

use std::collections::BTreeMap;

/// How a [`Event::Tag`] was delimited in the source.
///
/// Mirrors the reference's `ldelim` distinction: `@tag` (line command,
/// `ldelim == 0`, tag ends at end of line) vs `[tag]` (`ldelim == ']'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bracket {
    /// `@tag ...` — line-command mode; the tag spans to the end of the line.
    At,
    /// `[tag ...]` — square-bracket mode; the tag spans to the matching `]`.
    Square,
}

/// A single parsed scenario event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `*label` or `*label|macro`.
    ///
    /// `name` keeps the leading `*` (as in the reference label cache).
    /// `macro_name` is the part after `|`, if any.
    Label {
        name: String,
        macro_name: Option<String>,
        /// 1-based source line.
        line: usize,
    },
    /// A run of plain text with inline `[tags]` split out.
    Text {
        text: String,
        /// 1-based source line.
        line: usize,
    },
    /// `@tag ...` or `[tag ...]`.
    Tag {
        /// Lower-cased tag name.
        name: String,
        /// Ordered attribute list, `(name, value)`. Flag attributes
        /// (`name` without `=`) carry the value `"true"`.
        params: Vec<(String, String)>,
        bracket: Bracket,
        /// 1-based source line.
        line: usize,
    },
    /// `%directive ...` line. `raw` is the full line (leading tabs already
    /// stripped).
    Directive {
        name: String,
        args: Vec<String>,
        raw: String,
        /// 1-based source line.
        line: usize,
    },
    /// `;comment` line.
    Comment {
        text: String,
        /// 1-based source line.
        line: usize,
    },
}

/// The result of [`parse`]: an ordered event list plus a label index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    /// Parsed events, in source order.
    pub events: Vec<Event>,
    /// Label name (`*start`) → index into [`Scenario::events`] of the
    /// first occurrence.
    pub labels: BTreeMap<String, usize>,
}

/// Errors produced by [`parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The scenario contains no lines at all (reference `TVPKAGNoLine`).
    #[error("scenario is empty")]
    EmptyScenario,
    /// A line could not be parsed (reference `TVPKAGSyntaxError`).
    #[error("syntax error at line {line}: {message}")]
    Syntax { line: usize, message: &'static str },
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Parse a KAG scenario from decoded UTF-8 text.
///
/// See the [crate-level documentation](crate) for the exact syntax and for
/// the deliberate divergences from the reference C++ parser.
pub fn parse(source: &str) -> Result<Scenario> {
    let lines = split_lines(source);
    if lines.is_empty() {
        return Err(Error::EmptyScenario);
    }
    let mut events = Vec::new();
    let mut labels = BTreeMap::new();
    for (idx, line) in lines.iter().enumerate() {
        let lineno = idx + 1;
        // The reference strips leading tabs from every line (LoadScenario
        // pass 2) before classifying it.
        let content = line.trim_start_matches('\t');
        if content.is_empty() {
            // Empty line: the reference emits an "r" tag here; we emit
            // nothing.
            continue;
        }
        match content.chars().next() {
            Some(';') => events.push(Event::Comment {
                text: content[1..].to_owned(),
                line: lineno,
            }),
            Some('*') => {
                let (name, macro_name) = split_label(content);
                // First occurrence wins, mirroring the reference label
                // cache (which suffixes duplicates instead of overwriting).
                labels.entry(name.clone()).or_insert(events.len());
                events.push(Event::Label {
                    name,
                    macro_name,
                    line: lineno,
                });
            }
            Some('%') => {
                let (name, args) = parse_directive(content);
                events.push(Event::Directive {
                    raw: content.to_owned(),
                    name,
                    args,
                    line: lineno,
                });
            }
            _ => parse_line(content, lineno, &mut events)?,
        }
    }
    Ok(Scenario { events, labels })
}

/// Split the source into lines on `\r`, `\n` and `\r\n` (reference
/// `LoadScenario` pass 1/2). The final line without a trailing newline is
/// included; an empty source yields no lines at all.
fn split_lines(source: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut rest = source;
    while let Some(idx) = rest.find(['\r', '\n']) {
        lines.push(&source[start..start + idx]);
        let newline_len = match rest.as_bytes()[idx] {
            b'\r' if rest.as_bytes().get(idx + 1) == Some(&b'\n') => 2,
            _ => 1,
        };
        start += idx + newline_len;
        rest = &rest[idx + newline_len..];
    }
    if start < source.len() {
        lines.push(&source[start..]);
    }
    lines
}

/// Split a label line into `(name, macro_name)`. `name` is the part before
/// `|` (keeping the leading `*`, as in the reference label cache); the part
/// after `|` — if any — is the page/macro name.
fn split_label(line: &str) -> (String, Option<String>) {
    match line.find('|') {
        Some(pos) => (line[..pos].to_owned(), Some(line[pos + 1..].to_owned())),
        None => (line.to_owned(), None),
    }
}

/// Parse a `%directive ...` line into `(name, args)`. The name runs to the
/// first space or tab; the remainder is split on whitespace. The reference
/// C++ KAGParser has no `%` handling at all (in its output a leading `%`
/// would be ordinary text), so this follows the task spec: name and
/// arguments are preserved verbatim (not lower-cased).
fn parse_directive(line: &str) -> (String, Vec<String>) {
    let body = &line[1..];
    let name_end = body.find([' ', '\t']).unwrap_or(body.len());
    let name = body[..name_end].to_owned();
    let rest = body[name_end..].trim();
    let args = rest.split_whitespace().map(str::to_owned).collect();
    (name, args)
}

/// Parse the content of a non-comment, non-label, non-directive line:
/// either a single `@tag` line command, or a mix of text runs and inline
/// `[tag]`s.
fn parse_line(line: &str, lineno: usize, events: &mut Vec<Event>) -> Result<()> {
    let chars: Vec<char> = line.chars().collect();
    if chars.first() == Some(&'@') {
        // Line-command mode: `@tag ...` spans the whole line. `@` is only
        // special at the start of a line (the reference checks
        // `CurPos == 0`).
        let tag = parse_tag(&chars, 1, Bracket::At, lineno)?;
        events.push(Event::Tag {
            name: tag.name,
            params: tag.params,
            bracket: Bracket::At,
            line: lineno,
        });
        return Ok(());
    }

    let mut pos = 0;
    let mut text = String::new();
    while pos < chars.len() {
        match chars[pos] {
            '[' if pos + 1 < chars.len() && chars[pos + 1] == '[' => {
                // `[[` escapes a literal `[` (reference normal-character
                // branch).
                text.push('[');
                pos += 2;
            }
            '[' => {
                flush_text(&mut text, lineno, events);
                let tag = parse_tag(&chars, pos + 1, Bracket::Square, lineno)?;
                events.push(Event::Tag {
                    name: tag.name,
                    params: tag.params,
                    bracket: Bracket::Square,
                    line: lineno,
                });
                pos = tag.end;
            }
            '\t' => {
                // The reference emits no character tag for mid-line tabs.
                pos += 1;
            }
            c => {
                text.push(c);
                pos += 1;
            }
        }
    }
    flush_text(&mut text, lineno, events);
    Ok(())
}

/// Push a pending text run as an [`Event::Text`] (if non-empty).
fn flush_text(text: &mut String, lineno: usize, events: &mut Vec<Event>) {
    if !text.is_empty() {
        events.push(Event::Text {
            text: std::mem::take(text),
            line: lineno,
        });
    }
}

/// The parsed contents of a tag; `end` is the char index just past the
/// closing `]` (bracket mode) or the end of the line (line-command mode).
struct ParsedTag {
    name: String,
    params: Vec<(String, String)>,
    end: usize,
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t')
}

/// True for the tag terminator in bracket mode (`]`); always false in
/// line-command mode, where the end of the line terminates the tag.
fn is_ldelim(c: char, bracket: Bracket) -> bool {
    bracket == Bracket::Square && c == ']'
}

/// Parse a tag starting at `start` (just past the `[` or `@`), following
/// the reference `_GetNextTag` scanning rules. `line` is only used for
/// error reporting.
fn parse_tag(chars: &[char], start: usize, bracket: Bracket, line: usize) -> Result<ParsedTag> {
    let mut pos = start;
    let is_end = |p: usize| p >= chars.len();

    // --- tag name ---------------------------------------------------------
    while !is_end(pos) && is_ws(chars[pos]) {
        pos += 1;
    }
    if is_end(pos) {
        // `[` / `@` immediately at end of line.
        return Err(Error::Syntax {
            line,
            message: "unexpected end of line in tag",
        });
    }
    let name_start = pos;
    while !is_end(pos) && !is_ws(chars[pos]) && !is_ldelim(chars[pos], bracket) {
        pos += 1;
    }
    if pos == name_start {
        // e.g. `[]`
        return Err(Error::Syntax {
            line,
            message: "empty tag name",
        });
    }
    let name = chars[name_start..pos]
        .iter()
        .collect::<String>()
        .to_lowercase();

    // --- attributes -------------------------------------------------------
    let mut params = Vec::new();
    loop {
        while !is_end(pos) && is_ws(chars[pos]) {
            pos += 1;
        }
        if is_end(pos) {
            if bracket == Bracket::At {
                break; // line-command tag ends at end of line
            }
            return Err(Error::Syntax {
                line,
                message: "unterminated '[' tag (missing ']')",
            });
        }
        if bracket == Bracket::Square && chars[pos] == ']' {
            break; // tag ends
        }
        if chars[pos] == '*' {
            // "macro entity all" marker: consumed, never stored (the
            // reference expands macro arguments here).
            pos += 1;
            while !is_end(pos) && is_ws(chars[pos]) {
                pos += 1;
            }
            continue;
        }

        // attribute name
        let name_start = pos;
        while !is_end(pos)
            && !is_ws(chars[pos])
            && chars[pos] != '='
            && !is_ldelim(chars[pos], bracket)
        {
            pos += 1;
        }
        let attrib = chars[name_start..pos]
            .iter()
            .collect::<String>()
            .to_lowercase();
        while !is_end(pos) && is_ws(chars[pos]) {
            pos += 1;
        }

        let value = if is_end(pos) || chars[pos] != '=' {
            // Flag attribute: `name` without `=` means "true".
            "true".to_owned()
        } else {
            parse_attrib_value(chars, &mut pos, bracket, line)?
        };
        params.push((attrib, value));
    }

    let end = if bracket == Bracket::Square {
        pos + 1
    } else {
        pos
    };
    Ok(ParsedTag { name, params, end })
}

/// Parse an attribute value; `pos` must point at the `=`.
///
/// Returns the unescaped value and advances `pos` past the value (and past
/// a closing quote when present). Follows the reference `_GetNextTag` value
/// scan: optional `&`/`%` prefix, optional `"`/`'` quoting, backtick
/// escapes.
fn parse_attrib_value(
    chars: &[char],
    pos: &mut usize,
    bracket: Bracket,
    line: usize,
) -> Result<String> {
    *pos += 1; // consume '='
    if *pos >= chars.len() {
        return Err(Error::Syntax {
            line,
            message: "missing attribute value after '='",
        });
    }
    while *pos < chars.len() && is_ws(chars[*pos]) {
        *pos += 1;
    }
    if *pos >= chars.len() {
        return Err(Error::Syntax {
            line,
            message: "missing attribute value after '='",
        });
    }

    let quoted = matches!(chars[*pos], '"' | '\'');
    if quoted {
        let quote = chars[*pos];
        *pos += 1;
        let value_start = *pos;
        while *pos < chars.len() && chars[*pos] != quote {
            if chars[*pos] == '`' {
                *pos += 1;
                if *pos >= chars.len() {
                    return Err(Error::Syntax {
                        line,
                        message: "unterminated escape in attribute value",
                    });
                }
            }
            *pos += 1;
        }
        let value_end = *pos; // closing quote, or end of line
        if *pos < chars.len() {
            *pos += 1; // consume the closing quote
        }
        if value_end >= chars.len() && bracket == Bracket::Square {
            return Err(Error::Syntax {
                line,
                message: "unterminated quoted attribute value",
            });
        }
        // The reference tolerates an unclosed quote in `@` line-command
        // mode: the value simply runs to the end of the line.
        Ok(unescape(&chars[value_start..value_end]))
    } else {
        let value_start = *pos;
        while *pos < chars.len() {
            if chars[*pos] == '`' {
                *pos += 1;
                if *pos >= chars.len() {
                    return Err(Error::Syntax {
                        line,
                        message: "unterminated escape in attribute value",
                    });
                }
                *pos += 1;
            } else if !is_ws(chars[*pos]) && !is_ldelim(chars[*pos], bracket) {
                *pos += 1;
            } else {
                break;
            }
        }
        if bracket == Bracket::Square && *pos >= chars.len() {
            return Err(Error::Syntax {
                line,
                message: "unterminated '[' tag (missing ']')",
            });
        }
        Ok(unescape(&chars[value_start..*pos]))
    }
}

/// Remove backtick escapes from a raw attribute value: `` `a `` → `a`, and
/// a trailing backtick is dropped (reference `_GetNextTag` value unescape).
fn unescape(value: &[char]) -> String {
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < value.len() {
        if value[i] == '`' {
            i += 1;
            if i >= value.len() {
                break;
            }
        }
        out.push(value[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
            vec![tag("x", &[("a", "1")], Bracket::Square, 1)]
        );
        assert_eq!(events("[x *]"), vec![tag("x", &[], Bracket::Square, 1)]);
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
}
