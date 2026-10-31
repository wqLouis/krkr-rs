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
//! | `[` | square-bracket tag → [`Event::Tag`] with [`Bracket::Square`] |
//! | anything else | plain text → [`Event::Text`] (inline tags split out) |
//!
//! Only `@` and `[` open a tag, so a line that starts with `%` is ordinary
//! text: it is scanned for inline `[...]` tags like any other text line.
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
//! * `[iscript]`/`@iscript` … `[endscript]`/`@endscript` blocks are
//!   collected into a single [`Event::Script`] (the walk fires the owner's
//!   `onScript` as it passes it).
//! * The trailing-`\` and trailing-`[p]` line-continuation rules apply when
//!   `ignoreCR` is off: the state walk drops a trailing `\` and emits no `r`
//!   for a line ending in `[p]`
//!   (`reference/cpp/core/base/KAGParser.cpp:1415-1429`).
//! * Parsing is eager, but the syntax and first-label errors the reference
//!   raises during the walk are carried in [`Recovered`] and raised by the
//!   state walk when it reaches them; [`parse`]/[`parse_with_options`] surface
//!   the first one immediately for direct callers.
//! * `cond`, `if`/`elsif`/`else`, `emb`, `macro`, `jump`/`call`/`return`
//!   are all *parsed* but not *executed*: no expression evaluation, macro
//!   expansion or control flow happens here.
//! * Label names keep their leading `*` (the reference's label cache is
//!   keyed by `*name`, and `GoToLabel`/`CallLabel` receive `*`-prefixed
//!   names). Duplicate labels are suffixed `:2`, `:3`, …, and an empty
//!   label aliases the previous one, exactly like the reference label
//!   cache (`reference/cpp/core/base/KAGParser.cpp:193-238`).
//! * The KAGParserEx *multi-line tag* extension is available opt-in through
//!   [`ParseOptions::multiline_tags`]; [`parse`] leaves it off, matching the
//!   base parser.
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
    /// `;comment` line.
    Comment {
        text: String,
        /// 1-based source line.
        line: usize,
    },
    /// An `[iscript]`/`@iscript` … `[endscript]`/`@endscript` block, with
    /// its raw lines joined by `\r\n`. This is the reference
    /// `SkipCommentOrLabel` inline-script collection
    /// (`reference/cpp/core/base/KAGParser.cpp:1130-1176`); the owner's
    /// `onScript(script, storageShortName, scriptStart)` fires as the walk
    /// passes it.
    Script {
        /// The block's source lines, joined with `\r\n`.
        text: String,
        /// 0-based index of the first source line inside the block.
        start_line: usize,
        /// 1-based source line of the opening `[iscript]`/`@iscript`.
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
    /// Per-event scan column, aligned with [`Scenario::events`]. For a
    /// [`Event::Text`] it is the 0-based column where the run starts; for a
    /// [`Event::Tag`] it is the column just past the tag's closing `]`; the
    /// other variants carry 0. This is the reference `CurPos` scan cursor
    /// (`reference/cpp/core/base/KAGParser.cpp:1080-1082`).
    pub cols: Vec<usize>,
}

/// A parse error the reference raises lazily during the walk instead of at
/// load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingError {
    /// Number of events the walk emits before raising this error.
    pub after_event: usize,
    /// 1-based source line the reference is scanning when it raises.
    pub line: usize,
    /// The error to raise.
    pub error: Error,
}

/// Recovery-aware parse result: the events plus the errors the walk must
/// raise when it reaches them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// Parsed events.
    pub events: Vec<Event>,
    /// Label name (`*start`) → first-event index.
    pub labels: BTreeMap<String, usize>,
    /// Per-event scan columns, aligned with [`Recovered::events`].
    pub cols: Vec<usize>,
    /// Syntax errors the walk raises, ordered by `after_event`.
    pub pending_errors: Vec<PendingError>,
    /// A `*|page` before any named label; the reference raises it when it
    /// first builds the label cache.
    pub label_error: bool,
}

/// Errors produced by [`parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The scenario contains no lines at all (reference `TVPKAGNoLine`).
    #[error("scenario is empty")]
    EmptyScenario,
    /// An empty label (`*`) appears before any named label (reference
    /// `TVPKAGCannotOmmitFirstLabelName`).
    #[error("can not omit the first label name")]
    CannotOmitFirstLabelName,
    /// An `[iscript]` block has no matching `[endscript]` (reference
    /// `TVPKAGInlineScriptNotEnd`).
    #[error("inline script has no endscript")]
    InlineScriptNotEnd,
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
    let recovered = parse_recovering(source, options)?;
    if recovered.label_error {
        return Err(Error::CannotOmitFirstLabelName);
    }
    if let Some(pending) = recovered.pending_errors.first() {
        return Err(pending.error.clone());
    }
    Ok(Scenario {
        events: recovered.events,
        labels: recovered.labels,
        cols: recovered.cols,
    })
}

/// Recovery-aware parse used by the walk. Only an empty scenario is a load-time
/// error; syntax and label-cache errors are returned for the walk to raise when
/// it reaches them, matching the reference's lazy `EnsureLabelCache` and
/// `_GetNextTag` (`reference/cpp/core/base/KAGParser.cpp:194-238`,
/// `:1071-1108`, `:1367-1500`).
pub fn parse_recovering(source: &str, options: ParseOptions) -> Result<Recovered> {
    let raw_lines = split_lines(source);
    if raw_lines.is_empty() {
        return Err(Error::EmptyScenario);
    }
    // The reference strips leading tabs from every line (LoadScenario
    // pass 2) before classifying it; multi-line joining operates on the
    // tab-stripped text.
    let (lines, join_errors) = if options.multiline_tags {
        join_multiline_lines(&raw_lines)
    } else {
        (
            raw_lines
                .iter()
                .enumerate()
                .map(|(idx, line)| (idx + 1, line.trim_start_matches('\t').to_string()))
                .collect(),
            Vec::new(),
        )
    };
    let mut join_errors: BTreeMap<usize, Error> = join_errors.into_iter().collect();
    let mut events = Vec::new();
    let mut labels = BTreeMap::new();
    let mut cols = Vec::new();
    let mut pending_errors: Vec<PendingError> = Vec::new();
    let mut label_error = false;
    // Label name → how many times it has been stored in the cache. Keyed by
    // the name actually stored (base name or `base:N` alias), like the
    // reference `LabelCache` hash.
    let mut label_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut prev_label: Option<String> = None;
    let mut index = 0;
    while index < lines.len() {
        let (lineno, content) = &lines[index];
        let lineno = *lineno;
        if let Some(error) = join_errors.remove(&lineno) {
            // A bad multi-line continuation is a tag syntax error: the walk
            // raises it when it reaches the line (`_GetNextTag`).
            pending_errors.push(PendingError {
                after_event: events.len(),
                line: lineno,
                error,
            });
            index += 1;
            continue;
        }
        if content.is_empty() {
            // Empty line: the reference emits an "r" tag here; we emit
            // nothing.
            index += 1;
            continue;
        }
        // An inline script block is collected verbatim (its lines may
        // contain `[`/`]` as TJS), exactly like the reference
        // `SkipCommentOrLabel` (`reference/cpp/core/base/KAGParser.cpp:1130-1176`).
        if is_iscript_start(content) {
            let mut text = String::new();
            let mut end = index + 1;
            let mut found = false;
            while end < lines.len() {
                if is_iscript_end(&lines[end].1) {
                    found = true;
                    break;
                }
                text.push_str(&lines[end].1);
                text.push_str("\r\n");
                end += 1;
            }
            if !found {
                pending_errors.push(PendingError {
                    after_event: events.len(),
                    line: lineno,
                    error: Error::InlineScriptNotEnd,
                });
                break;
            }
            // `script_start` is the 0-based line of the first script line
            // (`CurLine` after the reference's `CurLine++`).
            events.push(Event::Script {
                text,
                start_line: lineno,
                line: lineno,
            });
            cols.push(0);
            index = end + 1;
            continue;
        }
        match content.chars().next() {
            Some(';') => {
                events.push(Event::Comment {
                    text: content[1..].to_owned(),
                    line: lineno,
                });
                cols.push(0);
            }
            Some('*') => {
                // The reference only caches a line as a label when it has at
                // least two characters (`EnsureLabelCache`,
                // `reference/cpp/core/base/KAGParser.cpp:204`). A bare `*`
                // line is not a cacheable label: the walk only clears the
                // current label, so it must neither alias the previous label
                // nor raise the first-label error.
                if content.chars().count() < 2 {
                    events.push(Event::Label {
                        name: String::new(),
                        macro_name: None,
                        line: lineno,
                    });
                    cols.push(0);
                } else {
                    let (mut name, macro_name) = split_label(content);
                    if name == "*" {
                        // A page-only label (`*|page`) has an empty name and
                        // aliases the previous one; without a previous label
                        // the reference raises `CannotOmitFirstLabelName`
                        // when it first builds the label cache
                        // (`KAGParser.cpp:215-219`), so the walk raises it.
                        let Some(previous) = prev_label.clone() else {
                            label_error = true;
                            events.push(Event::Label {
                                name: String::new(),
                                macro_name,
                                line: lineno,
                            });
                            cols.push(0);
                            index += 1;
                            continue;
                        };
                        name = previous;
                    }
                    prev_label = Some(name.clone());
                    // The reference keeps one cache entry per *stored* label
                    // name (`LabelCache`). A repeated name bumps that entry's
                    // count and stores the suffixed name `name:N`; a later
                    // label that literally spells a previous alias finds that
                    // alias as its own entry and bumps it again
                    // (`EnsureLabelCache`, KAGParser.cpp:224-232).
                    if let Some(count) = label_counts.get_mut(&name) {
                        *count += 1;
                        let suffix = *count;
                        name = format!("{name}:{suffix}");
                    }
                    label_counts.insert(name.clone(), 1);
                    labels.insert(name.clone(), events.len());
                    events.push(Event::Label {
                        name,
                        macro_name,
                        line: lineno,
                    });
                    cols.push(0);
                }
            }
            _ => {
                if let Err(error) = parse_line(content, lineno, &mut events, &mut cols) {
                    // The reference throws while scanning the line, after any
                    // text before the bad tag; record the error there.
                    pending_errors.push(PendingError {
                        after_event: events.len(),
                        line: lineno,
                        error,
                    });
                }
            }
        }
        index += 1;
    }
    Ok(Recovered {
        events,
        labels,
        cols,
        pending_errors,
        label_error,
    })
}

/// Whether a line opens an inline TJS script block (reference
/// `SkipCommentOrLabel`, `KAGParser.cpp:1130-1134`).
fn is_iscript_start(content: &str) -> bool {
    matches!(content, "[iscript]" | "[iscript]\\" | "@iscript")
}

/// Whether a line closes an inline TJS script block (reference
/// `SkipCommentOrLabel`, `KAGParser.cpp:1140-1145`).
fn is_iscript_end(content: &str) -> bool {
    matches!(content, "[endscript]" | "[endscript]\\" | "@endscript")
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

/// The joined `(line, content)` entries plus the deferred continuation
/// errors, keyed by the line they belong to.
type MultilineLines = (Vec<(usize, String)>, Vec<(usize, Error)>);

/// Join KAGParserEx multi-line tag continuations (see
/// [`ParseOptions::multiline_tags`]).
///
/// Every returned entry is `(line_number, joined_content)` where
/// `line_number` is the 1-based source line of the **first** line of the
/// logical tag. Continuation lines are folded into it, so the line count of
/// the result is smaller than the source line count.
fn join_multiline_lines(lines: &[&str]) -> MultilineLines {
    let mut out: Vec<(usize, String)> = Vec::with_capacity(lines.len());
    let mut errors: Vec<(usize, Error)> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let lineno = i + 1;
        let mut current = lines[i].trim_start_matches('\t').to_string();
        let mut joined = false;
        // A continuation marker is a trailing `\` preceded by whitespace
        // inside a tag's attribute area (KAGParserEx `readme.txt:16-38`): a
        // tag name must be present (`@ \` is the tag named `\`) and the `\`
        // must be outside any quoted value.
        while let Some(backslash) = tag_continuation_backslash(&current) {
            let Some(next_raw) = lines.get(i + 1) else {
                errors.push((
                    lineno,
                    Error::Syntax {
                        line: lineno,
                        message: "line continuation at end of file",
                    },
                ));
                break;
            };
            let next = next_raw.trim_start_matches('\t');
            let Some(rest) = next.strip_prefix(';') else {
                errors.push((
                    lineno,
                    Error::Syntax {
                        line: lineno,
                        message: "line continuation must be followed by a ';' line",
                    },
                ));
                break;
            };
            // Drop only the trailing `\`, keeping the whitespace before it
            // as the separator, then append the continuation line without
            // its leading `;`.
            let mut merged = current[..backslash].to_string();
            merged.push_str(rest);
            current = merged;
            i += 1;
            joined = true;
        }
        // Once a multi-line `[tag]` closes, everything after its `]` is
        // ignored and no line end is inserted
        // (`reference/cpp/plugins/KAGParser/readme.txt:36-42`).
        if joined
            && current.starts_with('[')
            && let Some(end) = first_bracket_end(&current)
        {
            current.truncate(end);
        }
        out.push((lineno, current));
        i += 1;
    }
    (out, errors)
}

/// The byte index of a KAGParserEx continuation `\` at the end of a tag line,
/// or `None`. A continuation needs whitespace before the `\`, a tag name
/// before it (`@ \` is the tag named `\`, not a continuation), and the `\`
/// outside any quoted value (`reference/cpp/plugins/KAGParser/readme.txt:16-38`).
fn tag_continuation_backslash(line: &str) -> Option<usize> {
    let trimmed = line.trim_end_matches([' ', '\t']);
    let backslash = trimmed.len().checked_sub(1)?;
    if trimmed.as_bytes().get(backslash) != Some(&b'\\') {
        return None;
    }
    let before = trimmed.as_bytes().get(backslash.checked_sub(1)?)?;
    if !matches!(*before, b' ' | b'\t') {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let opener = *bytes.first()?;
    if opener != b'@' && opener != b'[' {
        return None;
    }
    let mut i = 1;
    while i < backslash && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    let name_start = i;
    while i < backslash {
        match bytes[i] {
            b' ' | b'\t' => break,
            b']' if opener == b'[' => break,
            _ => i += 1,
        }
    }
    if i == name_start {
        return None;
    }
    // Reject a `\` that is inside an unterminated quoted value, or that
    // follows the tag's closing `]` (the attribute area has ended).
    let mut quote: Option<u8> = None;
    while i < backslash {
        let c = bytes[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) if c == b'`' => i += 1,
            Some(_) => {}
            None if c == b']' && opener == b'[' => return None,
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None => {}
        }
        i += 1;
    }
    if quote.is_some() {
        return None;
    }
    Some(backslash)
}

/// The byte index just past the first `]` of a joined KAGParserEx multi-line
/// bracket tag, ignoring `]` inside quotes or after a backtick escape. `None`
/// when the tag never closes
/// (`reference/cpp/plugins/KAGParser/readme.txt:36-42`).
fn first_bracket_end(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) if c == b'`' => i += 1,
            Some(_) => {}
            None if c == b'`' => i += 1,
            None if c == b'"' || c == b'\'' => quote = Some(c),
            None if c == b']' => return Some(i + 1),
            None => {}
        }
        i += 1;
    }
    None
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

/// Parse the content of a non-comment, non-label line: either a single
/// `@tag` line command, or a mix of text runs and inline `[tag]`s.
fn parse_line(
    line: &str,
    lineno: usize,
    events: &mut Vec<Event>,
    cols: &mut Vec<usize>,
) -> Result<()> {
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
        cols.push(tag.end);
        return Ok(());
    }

    let mut pos = 0;
    let mut text = String::new();
    let mut text_col = 0;
    while pos < chars.len() {
        match chars[pos] {
            '[' if pos + 1 < chars.len() && chars[pos + 1] == '[' => {
                // `[[` escapes a literal `[` (reference normal-character
                // branch).
                if text.is_empty() {
                    text_col = pos;
                }
                text.push('[');
                pos += 2;
            }
            '[' => {
                flush_text(&mut text, text_col, lineno, events, cols);
                let tag = parse_tag(&chars, pos + 1, Bracket::Square, lineno)?;
                events.push(Event::Tag {
                    name: tag.name,
                    params: tag.params,
                    macro_entity: tag.macro_entity,
                    bracket: Bracket::Square,
                    line: lineno,
                });
                cols.push(tag.end);
                pos = tag.end;
            }
            '\t' => {
                // The reference emits no character tag for mid-line tabs.
                pos += 1;
            }
            c => {
                if text.is_empty() {
                    text_col = pos;
                }
                text.push(c);
                pos += 1;
            }
        }
    }
    flush_text(&mut text, text_col, lineno, events, cols);
    Ok(())
}

/// Push a pending text run as an [`Event::Text`] (if non-empty), recording
/// its starting column in `cols`.
fn flush_text(
    text: &mut String,
    col: usize,
    lineno: usize,
    events: &mut Vec<Event>,
    cols: &mut Vec<usize>,
) {
    if !text.is_empty() {
        events.push(Event::Text {
            text: std::mem::take(text),
            line: lineno,
        });
        cols.push(col);
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

    // A `&`/`%` prefix may immediately precede a quoted value
    // (`key=&"expr"`). The reference consumes the prefix before it checks
    // for the quote delimiter, so the quote opens the value rather than being
    // part of it (`KAGParser.cpp:2067-2078`). Keep the prefix on the result:
    // the state layer strips one prefix and evaluates the unquoted content.
    let prefix = if matches!(chars[*pos], '&' | '%')
        && chars
            .get(*pos + 1)
            .is_some_and(|c| matches!(*c, '"' | '\''))
    {
        let p = chars[*pos];
        *pos += 1;
        Some(p)
    } else {
        None
    };

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
        let mut value = unescape(&chars[value_start..value_end]);
        if let Some(p) = prefix {
            value.insert(0, p);
        }
        Ok(value)
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
