//! KiriKiri **KAG** scenario (`.ks`) parser.
//!
//! This module re-implements the scenario-loading and tag-splitting half of
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
//!   all" marker. It is not stored as a parameter; its position is kept in
//!   the `macro_entity` field of [`Event::Tag`] so KAGParserEx can splice
//!   the caller's macro arguments in there during expansion.
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
//!   comment/label lines as pure control flow. This module is a *push*
//!   parser: one [`Event::Text`] per contiguous text run, one
//!   [`Event::Tag`] per tag, and empty lines produce no event (the
//!   reference emits an `r eol=true` tag).
//! * `[iscript]`/`@iscript` … `[endscript]`/`@endscript` blocks are parsed
//!   as ordinary tags (the reference consumes them and fires `onScript`).
//! * The trailing-`\` and trailing-`[p]` line-continuation rules only affect
//!   `r`-tag emission in the reference; since this module has no `r` events,
//!   a trailing `\` is kept in the text.
//! * `cond`, `if`/`elsif`/`else`, `emb`, `macro`, `jump`/`call`/`return`
//!   and `%`-directives are all *parsed* but not *executed*: no expression
//!   evaluation, macro expansion or control flow happens here.
//! * Label names keep their leading `*` (the reference's label cache is
//!   keyed by `*name`, and `GoToLabel`/`CallLabel` receive `*`-prefixed
//!   names).
//! * The KAGParserEx *multi-line tag* extension is available opt-in through
//!   [`ParseOptions::multiline_tags`]; [`parse`] leaves it off, matching the
//!   base parser.
//! * Duplicate labels: the reference suffixes duplicates (`name:2`, …) for
//!   jump disambiguation; this module records the first occurrence in
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
        /// Position (index into `params`) of the KAGParserEx `*`
        /// "macro entity all" marker, when one is present. The marker is
        /// *not* stored as a parameter; `Some(params.len())` means it was
        /// the last token. KAGParserEx splices the caller's macro arguments
        /// into the result at this position.
        macro_entity: Option<usize>,
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

/// Options controlling [`parse_with_options`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ParseOptions {
    /// Enable the KAGParserEx *multi-line tag* extension
    /// (`multiLineTagEnabled`): a tag line that ends with whitespace + `\`
    /// continues on the next line, which must begin with `;` (a comment to
    /// the base parser):
    ///
    /// ```text
    /// @tag hoge=123 fuga=test \
    /// ;    someoption=true \
    /// ;    andmore=false
    /// ```
    ///
    /// The trailing `\` and the leading `;` of each continuation line are
    /// removed and the lines are joined with the whitespace that preceded
    /// the `\`. A `\` not preceded by whitespace is *not* a continuation
    /// (it is part of the preceding value), and a continuation whose next
    /// line does not start with `;` is a syntax error, matching the
    /// reference KAGParserEx.
    pub multiline_tags: bool,
}

/// Parse a KAG scenario from decoded UTF-8 text with default options.
///
/// See the [crate-level documentation](crate) for the exact syntax and for
/// the deliberate divergences from the reference C++ parser.
pub fn parse(source: &str) -> Result<Scenario> {
    parse_with_options(source, ParseOptions::default())
}

/// Parse a KAG scenario from decoded UTF-8 text.
///
/// Identical to [`parse`] except that KAGParserEx features can be enabled
/// through [`ParseOptions`].
///
/// See the [crate-level documentation](crate) for the exact syntax and for
/// the deliberate divergences from the reference C++ parser.
pub fn parse_with_options(source: &str, options: ParseOptions) -> Result<Scenario> {
    let raw_lines = split_lines(source);
    if raw_lines.is_empty() {
        return Err(Error::EmptyScenario);
    }
    // The reference strips leading tabs from every line (LoadScenario
    // pass 2) before classifying it; multi-line joining operates on the
    // tab-stripped text.
    let lines: Vec<(usize, String)> = if options.multiline_tags {
        join_multiline_lines(&raw_lines)?
    } else {
        raw_lines
            .iter()
            .enumerate()
            .map(|(idx, line)| (idx + 1, line.trim_start_matches('\t').to_string()))
            .collect()
    };
    let mut events = Vec::new();
    let mut labels = BTreeMap::new();
    for (lineno, content) in &lines {
        let lineno = *lineno;
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

/// Join KAGParserEx multi-line tag continuations (see
/// [`ParseOptions::multiline_tags`]).
///
/// Every returned entry is `(line_number, joined_content)` where
/// `line_number` is the 1-based source line of the **first** line of the
/// logical tag. Continuation lines are folded into it, so the line count of
/// the result is smaller than the source line count.
fn join_multiline_lines(lines: &[&str]) -> Result<Vec<(usize, String)>> {
    let mut out: Vec<(usize, String)> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let lineno = i + 1;
        let mut current = lines[i].trim_start_matches('\t').to_string();
        loop {
            // A continuation marker is a trailing `\` that is preceded by
            // at least one whitespace character (`@tag foo=1 \`).
            let trimmed = current.trim_end_matches([' ', '\t']);
            let is_continuation = trimmed.ends_with('\\')
                && trimmed.len() >= 2
                && trimmed[..trimmed.len() - 1].ends_with([' ', '\t']);
            if !is_continuation {
                break;
            }
            let Some(next_raw) = lines.get(i + 1) else {
                return Err(Error::Syntax {
                    line: lineno,
                    message: "line continuation at end of file",
                });
            };
            let next = next_raw.trim_start_matches('\t');
            let Some(rest) = next.strip_prefix(';') else {
                return Err(Error::Syntax {
                    line: lineno,
                    message: "line continuation must be followed by a ';' line",
                });
            };
            // Drop only the trailing `\`, keeping the whitespace before it
            // as the separator, then append the continuation line without
            // its leading `;`.
            let mut merged = trimmed[..trimmed.len() - 1].to_string();
            merged.push_str(rest);
            current = merged;
            i += 1;
        }
        out.push((lineno, current));
        i += 1;
    }
    Ok(out)
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
            macro_entity: tag.macro_entity,
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
                    macro_entity: tag.macro_entity,
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
    /// See the `macro_entity` field of [`Event::Tag`].
    macro_entity: Option<usize>,
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
    let mut macro_entity: Option<usize> = None;
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
            // "macro entity all" marker: remembered by position but never
            // stored as a parameter; KAGParserEx splices the caller's macro
            // arguments in here during expansion.
            if macro_entity.is_none() {
                macro_entity = Some(params.len());
            }
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
    Ok(ParsedTag {
        name,
        params,
        macro_entity,
        end,
    })
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
///
/// A backtick that escapes a *leading* `&` or `%` is kept in the result as
/// a one-character marker (`&foo`), so the state layer can tell an escaped
/// entity/macro-argument prefix from an active one. The reference tracks the
/// same distinction in separate `entity`/`macroarg` flags (the core
/// `KAGParser.cpp` sets them before stripping backticks).
fn unescape(value: &[char]) -> String {
    let escaped_prefix = value.first() == Some(&'`');
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
    if escaped_prefix && matches!(out.chars().next(), Some('&' | '%')) {
        out.insert(0, '`');
    }
    out
}
