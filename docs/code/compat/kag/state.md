# `compat::natives::kag::state` — the pull state machine

Source: `crates/compat/src/natives/kag/state.rs` · layer **L3** · no script
class of its own

`KagParserState` is the stateful half of the KAG parser. It owns the parsed
scenario, the walk position, the macro / conditional / call machinery, and it
answers the native class's `getNextTag()`, `goToLabel()`, `callLabel()`,
`store()` and `restore()` calls.

It is a **pull** wrapper around [`parser.rs`](parser.md)'s **push** parser:
the scenario is parsed once into events, and the walk hands them out one tag
at a time — the shape the reference's `_GetNextTag` has.

---

## 1. The environment

The state machine never touches the VM or the storage itself. Both arrive
through an injectable `Environ`:

```rust
pub struct Environ<'a> {
    pub eval: &'a mut dyn FnMut(&str) -> Result<EvalResult, String>,
    pub load_storage: &'a mut dyn FnMut(&str) -> Result<String, String>,
    pub fire_label: Option<LabelCallback<'a>>,
    pub fire_script: Option<ScriptCallback<'a>>,
    pub fire_jump: Option<TagCallback<'a>>,
    pub fire_call: Option<TagCallback<'a>>,
    pub fire_return: Option<TagCallback<'a>>,
    pub fire_scenario_load: Option<ScenarioLoadCallback<'a>>,
    pub fire_scenario_loaded: Option<ScenarioLoadedCallback<'a>>,
}
```

That is what makes the machine unit-testable without a VM: the test suite
supplies its own `eval` and `load_storage`.

`EvalResult` is `Void | Integer | Real | Str | Object { text, truthy }`. An
object result carries the **string form** of the already-evaluated object
(`'' + v`) and its `operator bool` truthiness, because the object itself
cannot cross the boundary.

---

## 2. State it owns

| group | fields |
|---|---|
| scenario | `events`, `labels`, `raw_lines`, `event_cols`, `comment_or_label_lines`, `storage_name`, `storage_short_name`, `label_error`, `pending_errors` |
| walk position | `event_index`, `char_index`, `cur_line`, `cur_pos`, `pending_rs`, `splice_stack` |
| current label | `cur_label`, `cur_page` |
| macros | `macros`, `param_macros`, `macro_args`, `macro_args_base`, `recording_macro`, `recording_macro_name`, `recording_macro_str` |
| conditionals | `exclude_level`, `if_level`, `exclude_level_stack`, `if_level_executed_stack` |
| call stack | `call_stack` |
| flags | `interrupted`, `ignore_cr`, `process_special_tags`, `multi_line_tag_enabled`, `debug_level` |

The defaults follow the reference constructor: `process_special_tags` on,
`debug_level` 1 (`tkdlSimple`), `exclude_level` −1 (not excluded),
`multi_line_tag_enabled` off.

---

## 3. The walk

`next_tag()` returns `Option<(String, Vec<(String, String)>)>` — the tag name
and its resolved attributes — or `None` at end of scenario.

The reference's tag stream is reproduced:

| tag | when |
|---|---|
| `interrupt` | once, after `interrupt()` |
| `ch` | one character of text, attribute `text` |
| `r` | end of line, attribute `eol="true"` |
| the tag's own name | an ordinary scenario tag |

`r` is suppressed when `ignoreCR` is on. A line that ends with a trailing
`\`, or with the `[p]` marker, produces **no** `r` (the reference's
continuation rules). A line that is only a comment or a label produces no `r`
either.

Text inside an excluded (false `if`/`ignore`) region is consumed silently; so
is a line end inside such a region.

### Control tags

With `process_special_tags` on, the machine executes these itself and returns
nothing for them:

`if`, `ignore`, `elsif`, `else`, `endif`, `endignore`, `emb`, `macro`,
`endmacro`, `macropop`, `erasemacro`, `pmacro`, `erasepmacro`, `jump`,
`call`, `return`.

* **`if` / `ignore`** take their condition from the `exp` attribute; `ignore`
  inverts it. A missing or empty `exp` is a syntax error. `cond="…"` on any
  tag is consumed as a condition and is not stored as an attribute.
* **`elsif` / `else`** select the branch only if no earlier branch of the
  same `if` was taken.
* **`endif` / `endignore`** restore the enclosing exclude level.
* **`emb`** evaluates its `exp` and splices the result into the stream. The
  `escape` attribute (default true) decides whether the result is inserted as
  plain text or re-parsed as tags. `escape` is coerced with TJS `operator
  bool`, so `escape=false` and `escape=0` are false while a bare flag is
  true.
* **`macro`** starts a recording (the `name` attribute is required);
  **`endmacro`** ends it and stores the body. A `[macro]` nested inside a
  recording clears the recording name (a reference quirk).
* **`macropop`** pops one macro-argument level; popping an empty stack is a
  syntax error.
* **`erasemacro` / `erasepmacro`** remove a macro; removing an unknown macro
  raises `Unknown macro "<name>"`.
* **`pmacro`** stores the tag's remaining attributes as the macro's
  parameter list (the `name` attribute is excluded).
* **`jump` / `call` / `return`** take `storage` and `target` and consult the
  owner's veto callback first.

### Attribute resolution

`entry_param` resolves one attribute:

1. if the attribute name is registered in `paramMacros`, the registered
   `(name, value)` pairs are spliced in recursively and the attribute itself
   is **not** stored;
2. otherwise a leading `&` means "evaluate this expression" and a leading `%`
   means "take this macro argument";
3. `cond` is consumed as the tag's condition and never stored;
4. everything else is appended, keeping the source order. A repeated
   attribute name keeps its **first** position and takes the **last** value.

A value that resolves to void **omits** the attribute entirely.

---

## 4. Macros

A macro body is recorded by **reconstructing** the tags
(`[name key=value …]`, re-quoting values that would not re-parse) rather than
copying the raw source. The recorded body re-parses to the same events, which
is what the expansion path needs.

When a macro tag is reached, the body is parsed, the caller's attributes are
pushed as a new macro-argument level, and the parsed body is spliced ahead of
the main stream. The `*` marker splices the caller's macro arguments in at
that position.

`%name` resolves against the innermost macro-argument level; `%name|default`
falls back to the default. Later entries win. With no macro arguments at all,
the value is kept as-is (without the `%`).

`macroParams` / `mp` expose the innermost level; `macroArgs` keeps them all.

---

## 5. The call stack

`callLabel` pushes a frame (the storage, the label, the offset, the original
line text, the cursor position, the active expansion buffer, the macro-argument
base and depth, and the conditional levels) and then jumps.

`return` pops a frame and resumes there. A `return` with an empty stack
raises `'return' is not matched to any 'call' ( 'return' is unexpected )`; a
frame whose scenario no longer matches raises `Lost return position due to
the scenario file changed.`

`clearCallStack` empties the stack.

---

## 6. Position

| accessor | value |
|---|---|
| `cur_line()` | the current line, **0-based** (the reference's `CurLine`) |
| `cur_pos()` | the current column, 0-based |
| `cur_line_str()` | the current line's text, or the active expansion buffer |
| `line_buffer()` / `line_buffer_using()` | the active expansion buffer and whether one is in use |
| `storage_name()` / `storage_short_name()` | the loaded scenario's name and its basename |
| `cur_label()` | the label the walk is on |
| `call_stack_depth()` | the number of pushed frames |
| `exclude_level()` / `if_level()` | the conditional levels |
| `macro_args_base()` / `macro_args_depth()` | the macro-argument bookkeeping |

The reference's `CurLine` is 0-based and so is this one; the parser's own
event line numbers are 1-based, and the state machine converts between them.

---

## 7. `store` / `restore`

`store()` serializes the whole state into an ordered `(key, value)` list and
encodes it with the [`kvp`](kvp.md) codec. The keys include the scenario
identity, the walk position, the flags, the conditional levels and their
stacks, the macro-argument levels, the macro and parameter-macro tables, the
call-stack frames and the live expansion splices.

`restore(s, env)` decodes that list, re-loads the scenario from storage and
rewinds to `curLabel`, like the reference `Restore`
(`reference/cpp/core/base/KAGParser.cpp:723-906`). It re-applies the
reference restore fields — macros, macro arguments, call-stack frames,
conditional levels and their stacks, and the macro-argument base — but it
does **not** re-apply the saved current walk position (`eventIndex`,
`charIndex`, `curLine`, `curPos`) or the active expansion buffer. A scenario
saved while paused on a tag runs that tag again after load.

`restore_decoded(entries, env)` is the same over already-decoded entries; the
native uses it to import a reference-format dictionary.

The stack encodings (`encode_int_stack`, `encode_bool_stack`) are the
reference's own: the integer stack is hex-encoded and the boolean stack uses
`"01"`.

---

## 8. Errors

The machine raises the reference's scenario messages (quoted in
[`../kag.md`](../kag.md) §7) plus:

| message | condition |
|---|---|
| `Unknown macro "<name>"` | `erasemacro` / `erasepmacro` for an unknown macro |
| `Syntax error.…` | a missing `exp` attribute, an `endmacro` without a recording, a `macropop` on an empty stack, a `[macro]` without a `name` |

A parser error that the reference raises lazily is carried in
`pending_errors` and raised by `next_tag` when the walk reaches the event
index it belongs to — so the tags before it are still delivered.

---

## 9. Deliberate divergences from the reference

1. **The reference emits one `ch` per character and one `r` per line end**;
   the port keeps that shape even though the parser underneath works in whole
   text runs. The empty lines the parser drops are recovered from the line
   numbers.
2. **The owner callbacks are supplied through `Environ`.** A missing handler
   or a handler error leaves the tag processed, matching the reference's
   default.
3. **Conditions and `emb` expressions are evaluated globally**, not in the
   owner's context.
4. **Macro recording reconstructs the body** from the parsed events; the `*`
   macro-entity marker is lost in the recorded body (it survives in the
   expansion path), and `&`/`%` prefixes survive in the values.
5. **`assign` does not copy `process_special_tags` or `interrupted`**, which
   is what the reference's copy leaves with the destination.

---

## 10. Tests

`crates/compat/tests/kag.rs` compiles this module standalone and drives it
with an injected environment: the text/line-end tag stream, `curLine`/
`curPos`, `ignoreCR`, the `if`/`elsif`/`else`/`endif` machinery, `ignore`
regions, macro recording and expansion, `emb` splicing, `jump`/`call`/
`return`, the veto callbacks, `interrupt`, `assign`, `clear`, the
`store`/`restore` label rewind, the `store` dictionary's shape, and the
reference-format import.
