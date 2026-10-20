# `compat::natives::kag::parser` — the scenario text parser

Source: `crates/compat/src/natives/kag/parser.rs` · layer **L3** · no script
class of its own

This module turns decoded scenario text into an **ordered event list** plus a
label index. It is a pure transform: no VM, no storage, no engine crate — it
depends on `std` and `thiserror` only, which is why the test suite compiles
it standalone.

It is a **push** parser. The pull behaviour the script sees
(`getNextTag()` returning one tag at a time) lives in
[`state.rs`](state.md).

---

## 1. Public API

```rust
pub fn parse(source: &str) -> Result<Scenario>;
pub fn parse_with_options(source: &str, options: ParseOptions) -> Result<Scenario>;
pub fn parse_recovering(source: &str, options: ParseOptions) -> Result<Recovered>;

pub enum Bracket { At, Square }
pub enum Event { Label, Text, Tag, Directive, Comment, Script }
pub struct Scenario { pub events: Vec<Event>, pub labels: BTreeMap<String, usize>, pub cols: Vec<usize> }
pub struct Recovered { pub events, pub labels, pub cols, pub pending_errors: Vec<PendingError>, pub label_error: bool }
pub struct ParseOptions { pub multiline_tags: bool }
pub enum Error { EmptyScenario, CannotOmitFirstLabelName, InlineScriptNotEnd, Syntax { line, message } }
```

`parse` and `parse_with_options` raise the **first** recovered error
immediately. `parse_recovering` returns the events together with the errors
the walk must raise when it reaches them — that is what the state machine
uses, so a scenario with a syntax error late in the file still yields its
earlier tags first.

---

## 2. The line model

The source is split on `\r`, `\n` and `\r\n`; the final line without a
trailing newline is included. An empty source yields **no lines at all**, which
is an error.

Leading tabs are stripped from every line. Each line is then classified by
its first character:

| first char | event |
|---|---|
| `;` | `Comment` |
| `*` | `Label` |
| `@` | `Tag` with `Bracket::At` |
| `%` | `Directive` |
| `[` | `Tag` with `Bracket::Square` (or a `Script` block) |
| anything else | `Text` |

An empty line produces **no event** (the reference emits a line-end tag
there; the state machine reconstructs those).

A line that is exactly `[iscript]`, `[iscript]\` or `@iscript` opens an
inline script block: the following lines are collected verbatim (joined with
`\r\n`) until `[endscript]`, `[endscript]\` or `@endscript`, and become one
`Script` event. A block with no end is `Error::InlineScriptNotEnd`.

---

## 3. Tag syntax

`@tag …` is a **line command**: it spans the rest of the line and needs no
closing bracket. `[tag …]` is **bracket mode**: it ends at the matching `]`.
`@` is only special at the start of a line.

* The **tag name** and every **attribute name** are lower-cased.
* Attributes are `name=value`, `name="quoted"`, `name='quoted'` or a bare
  `name` (a flag, whose value is `"true"`).
* A quoted value may contain spaces and `]`; an unquoted value runs to the
  next whitespace (or to `]` in bracket mode).
* A backtick escapes the following character anywhere inside a value, quoted
  or not, and is removed from the result.
* A `&`/`%` value prefix is **preserved verbatim** — this parser does not
  evaluate it. The state layer consumes it.
* A `&`/`%` prefix may sit immediately before a quote (`key=&"expr"`); the
  prefix is consumed first, so the quote opens the value.
* A lone `*` in the attribute position is the "macro entity all" marker. It
  is **not** stored as a parameter; its position is kept in
  `Event::Tag::macro_entity` so the state layer can splice the caller's
  macro arguments in there during expansion.
* In plain text, `[[` is an escape for a literal `[`; any other `[` opens an
  inline tag.
* Mid-line tabs are dropped from text.

### Syntax errors

| condition | error |
|---|---|
| `[` or `@` at end of line | `Syntax { message: "unexpected end of line in tag" }` |
| `[]` — an empty tag name | `Syntax { message: "empty tag name" }` |
| a bracket tag with no `]` | `Syntax { message: "unterminated '[' tag (missing ']')" }` |
| `name=` with no value | `Syntax { message: "missing attribute value after '='" }` |
| an unterminated quoted value in bracket mode | `Syntax` |
| an unterminated escape | `Syntax` |
| a bare `@` line | `Syntax` |

An unterminated quote is **tolerated in line-command mode** (the tag simply
ends at the end of the line) and is an error in bracket mode.

---

## 4. Labels

The label name keeps its leading `*` — the reference's label cache is keyed by
`*name`, and `goToLabel`/`callLabel` receive `*`-prefixed names.

* A label line of one character (`*` alone) is **not** a cacheable label: it
  clears the current label and neither aliases the previous one nor raises
  the first-label error.
* `*|page` is a **page-only** label: it has an empty name and aliases the
  previous label. With no previous label it raises
  `Error::CannotOmitFirstLabelName` — but lazily, when the walk first builds
  the label cache.
* A repeated name is suffixed `:2`, `:3`, … and each *stored* name is its own
  cache entry. A later label that literally spells a previous alias finds
  that alias as its own entry and bumps it again.

---

## 5. Columns

`Scenario::cols` is aligned with `Scenario::events`:

* for a `Text` event it is the 0-based column where the run starts;
* for a `Tag` event it is the column just past the tag's closing `]`
  (the end of the line for a line command);
* every other variant carries `0`.

This is the reference's `CurPos` scan cursor.

---

## 6. The multi-line tag extension

`ParseOptions::multiline_tags` enables the KAGParserEx extension. A tag line
that ends with whitespace followed by `\` continues on the next line, which
must begin with `;`:

```text
@tag hoge=123 fuga=test \
;    someoption=true \
;    andmore=false
```

The trailing `\` and the leading `;` of each continuation line are removed and
the lines are joined with the whitespace that preceded the `\`. A `\` **not**
preceded by whitespace is not a continuation (it is part of the preceding
value), and a continuation whose next line does not start with `;` is a
syntax error.

`parse` leaves the extension **off**, matching the base parser. The native
class exposes it through the `multiLineTagEnabled` property.

---

## 7. Deliberate divergences from the reference

These are the differences a reader of the reference C++ will notice:

1. **Push, not pull.** The reference returns one character per `ch` tag and
   emits an `r` tag per line break. This module produces whole text runs and
   no line-end events; [`state.rs`](state.md) reconstructs the reference's
   tag stream from the event list and the line numbers.
2. **Empty lines produce no event** (the reference emits `r eol=true`).
3. **`[iscript]`/`@iscript` blocks are collected** into one `Script` event
   rather than being walked character by character.
4. **Control tags are parsed, not executed.** `cond`, `if`/`elsif`/`else`,
   `emb`, `macro`, `jump`/`call`/`return` and `%`-directives are recognised
   but nothing is evaluated here.
5. **Line numbers are 1-based** in the events; the reference's `curLine` is
   0-based and the state machine keeps that distinction.
6. **Syntax and first-label errors are deferred** to the walk.

---

## 8. Tests

`crates/compat/tests/kag.rs` compiles this module directly and asserts the
line model, the tag syntax, the quoting and escaping rules, the label rules,
the directive and comment handling, the inline-script collection, every
syntax error (with the reported line), and the multi-line extension in both
modes.
