# `compat::natives::kag` — the KAG scenario parser

Source: `crates/compat/src/natives/kag/` · layer **L3** · class `KAGParser`

`KAGParser` is the class a game's KAG runtime talks to: it loads a `.ks`
scenario out of the storage namespace and hands the script one tag at a
time. The class is the *native adapter*; the work happens in three modules
underneath it.

| module | role | document |
|---|---|---|
| `kag/mod.rs` | the native adapter: registers `KAGParser`, marshals TJS values, owns the per-instance payload | this page |
| `kag/parser.rs` | the **push** parser: text → an ordered event list + a label index | [kag/parser.md](kag/parser.md) |
| `kag/state.rs` | the **pull** state machine: the event list → one tag per `getNextTag()` call, plus the macro / conditional / call machinery | [kag/state.md](kag/state.md) |
| `kag/kvp.rs` | the string codec for the dictionary-shaped data the parser keeps as strings | [kag/kvp.md](kag/kvp.md) |

The KAG layer semantics themselves (the `Window`/`Layer`/`KAG` tag handlers in
`kag.tjs`) are ordinary TJS scripts, not native code. This page covers only
the native half.

---

## 1. Registered surface

`KAGParser` is an **instance** class: one native payload per object, a
`KAGParser` constructor member, twelve methods and fourteen properties.

### Methods

| member | behaviour |
|---|---|
| `KAGParser()` | constructor; the payload already exists, so the member is empty and returns void |
| `loadScenario(storageName)` | loads a scenario from storage (or from the owner's `onScenarioLoad` override) and rewinds the walk |
| `getNextTag()` | the next tag as a Dictionary, or **void** at end of scenario |
| `goToLabel(label)` | jumps to a `*label`; the name keeps the leading `*` |
| `callLabel(label)` | pushes the current position on the call stack and jumps |
| `assign(src)` | copies another `KAGParser`'s walk state |
| `clear()` | drops the scenario and the per-scenario state; macros survive |
| `store()` | serializes the parser state into a Dictionary |
| `restore(dict)` | restores from a `store()` Dictionary or from a reference-format Dictionary |
| `clearCallStack()` | empties the call stack |
| `popMacroArgs()` | pops the innermost macro-argument level |
| `interrupt()` | makes the next `getNextTag()` return an `interrupt` tag |
| `resetInterrupt()` | clears a pending interrupt |

### Properties

| member | access | value |
|---|---|---|
| `curStorage` | read/write | the loaded scenario's storage name; the **setter loads that scenario** |
| `curLabel` | read-only | the label the walk is on |
| `curLine` | read-only | the current line, **0-based** (the reference's `CurLine`) |
| `curPos` | read-only | the current column, 0-based |
| `curLineStr` | read-only | the current line's text, or the active expansion buffer |
| `callStackDepth` | read-only | the number of pushed call frames |
| `ignoreCR` | read/write | suppress the line-end (`r eol=true`) tags |
| `processSpecialTags` | read/write | when on, the parser executes `if`/`macro`/`jump`/… itself instead of returning them |
| `multiLineTagEnabled` | read/write | the KAGParserEx multi-line tag extension |
| `debugLevel` | read/write | the reference's `tTVPKAGDebugLevel`; the setter uses the strict integer conversion |
| `macros` | read-only | the macro table as a Dictionary |
| `paramMacros` | read/write | the KAGParserEx parameter-macro table as a Dictionary |
| `macroParams` | read-only | the innermost macro-argument level as a Dictionary, or `null` |
| `mp` | read-only | the same value as `macroParams` |

The surface dump is [`../../surface/runtime_surface.json`](../../surface/runtime_surface.json);
the lock and what it means are described in
[`../../arch/l3.md`](../../arch/l3.md) §7. `finalize` in the dump is the TJS2 base
`Object.finalize`, not a member of this class.

---

## 2. Object results

The reference's `getNextTag` returns a *string encoding* of the tag
dictionary. The port returns **real TJS objects** instead, because the VM can
retain an object and the C++ trampoline consumes a `VAL_RETAINED` result:

* `getNextTag()` returns a Dictionary with `tagname`, every attribute in
  source order, and `taglist` — an array of the member names in insertion
  order (`tagname` first). The array literal is built from escaped attribute
  names, so **no scenario text is ever evaluated as TJS**.
* `macros` returns a Dictionary of macro name → body.
* `macroParams` / `mp` return the innermost macro-argument level as a
  Dictionary, or `null` when the stack is empty.
* `paramMacros` returns a Dictionary of macro name → the flattened
  `[name, value, name, value, …]` array.
* `store()` returns a Dictionary whose `data` member holds the emulator
  snapshot `restore()` reads, alongside the reference's own top-level
  members. The snapshot records the walk position, but `restore()` follows
  the reference and does not apply it.

`macros` and `macroParams` are **cached**: the same Dictionary object is
handed back on every read while its content is unchanged, and it is rebuilt
when the content changes. The reference hands the same object out too.

`macros` is **read-only** (a write is denied, like the reference). The live
table is shared with the script: the walk reads back script mutations before
each `getNextTag()`, and `@macro` / `@erasemacro` write into it.

---

## 3. The walk

`getNextTag()` is a **pull**: each call returns the next tag and advances the
walk. The tags it can return are:

| tag | meaning |
|---|---|
| `ch` | one character of text, attribute `text` |
| `r` | end of line, attribute `eol="true"` (suppressed when `ignoreCR` is on) |
| the scenario tag's own name | an ordinary tag, with its resolved attributes |
| `interrupt` | the tag produced by `interrupt()` |

The reference emits one `ch` per character and one `r` per line end; the port
keeps that shape even though the parser underneath works in whole text runs.

### Control tags

When `processSpecialTags` is on (the default), the parser executes these tags
itself and returns nothing for them:

`if`, `ignore`, `elsif`, `else`, `endif`, `endignore`, `emb`, `macro`,
`endmacro`, `macropop`, `erasemacro`, `pmacro`, `erasepmacro`, `jump`,
`call`, `return`.

Conditions and `emb` expressions are evaluated by calling back into the VM
through the environment hook — **globally**, not in the owner's context.
`jump` / `call` / `return` ask the owner first; the owner may veto the
transition.

### Attribute resolution

An attribute value may carry a leading `&` (a TJS expression) or `%` (a macro
argument). The prefix is consumed and the rest is resolved:

* `&expr` → the expression is evaluated in the VM and the result is
  stringified. A void result omits the attribute entirely.
* `%name` → the value is taken from the macro-argument level.
* A backtick before the prefix (`` `& `` / `` `% ``) keeps the `&`/`%`
  literal.

A `*` in the attribute position is the "macro entity all" marker: it is not
stored as a parameter, and the caller's macro arguments are spliced in at
that position during expansion.

---

## 4. Owner callbacks

The object's own members are the owner hooks. Each is optional; a missing
handler, or a handler that raises, leaves the tag processed — matching the
reference's defaults.

| member | fires |
|---|---|
| `onScenarioLoad(name)` | before a scenario is read; a **string** return replaces the scenario text |
| `onScenarioLoaded(name)` | after a scenario is read |
| `onLabel(label, pageName)` | as the walk passes a label line |
| `onScript(script, storageShortName, scriptStart)` | as the walk passes an `[iscript]` block |
| `onJump(tag, params)` | on a `jump`; returning a falsy value vetoes the jump |
| `onCall(tag, params)` | on a `call`; returning a falsy value vetoes the call |
| `onReturn(tag, params)` | on a `return`; returning a falsy value vetoes the return |

`onJump` / `onCall` / `onReturn` receive the tag name and the resolved
attribute list, and their result is coerced with TJS `operator bool` (a
string is truthy iff its integer value is non-zero).

---

## 5. State ownership

| state | owner | note |
|---|---|---|
| the parsed scenario (events, labels, raw lines, columns) | the **native instance payload** | one per `KAGParser` object |
| the walk position, macro tables, conditional levels, call stack | the **native instance payload** | `KagParserState` |
| the cached `macros` / `macroParams` Dictionaries | the **native instance payload** | identity-stable while unchanged |
| the scenario text | the **L2 storage namespace** | read through `engine_api::Storage` |
| the `out.string` scratch buffer | the **per-engine dispatch state** | `CompatServices::state`, not a thread-local |

The payload is allocated by `kagparser_create` and freed by
`kagparser_destroy`. The VM's `invalidate` hook clears the cached
dictionaries, the walk state and the macro-argument stack, so no walk state
survives to fire a callback from.

There is no process-wide state beyond the documented engine→context binding
the C ABI forces.

---

## 6. The storage seam

`loadScenario` reads through the attached L2 storage:

```
script name
  → strip the `file:` / `file://` / `file://.` media prefix
  → engine_api::Storage::read
  → decode_text(bytes, Detect)      (BOM → charset detection)
  → the scenario text
```

A read or decode failure raises. If the owner implements `onScenarioLoad` and
returns a string, that text is used **instead** of the storage read.

The `>` archive delimiter and the `file` media prefix are L3 grammar: the L2
namespace never sees them. The storage rules are in
[`storages.md`](storages.md).

---

## 7. Errors

Every failure raises a TJS error. The message vocabulary:

| message | raised by |
|---|---|
| `KAGParser.loadScenario requires 1 argument` | a missing argument |
| `KAGParser.goToLabel requires 1 argument` | a missing argument |
| `KAGParser.callLabel requires 1 argument` | a missing argument |
| `KAGParser.assign requires 1 argument` | a missing argument |
| `KAGParser.restore requires 1 argument` | a missing argument |
| `KAGParser: compat context is not bound` | the engine binding is missing |
| `KAGParser: no storage is attached to the compat services` | no L2 storage |
| `Please specify KAGParser object.` | `assign` with a non-`KAGParser` argument (the reference's exact text) |
| `KAGParser.restore: please specify the dictionary returned by store()` | `restore` with a non-object |
| `Malformed savedata, data may damaged.` | a reference-format save whose macro-argument depth does not match its array |
| `KAGParser.paramMacros: please specify a dictionary` | a non-object `paramMacros` |
| `KAGParser.paramMacros: <name> is not an array (…)` | a `paramMacros` value that is not an array |
| `KAGParser.paramMacros: <name> has a negative parameter count` | a negative `count` |
| `KAGParser.paramMacros: <name> has an odd-length parameter list` | an odd element count |
| `KAGParser.paramMacros: <name> has too many parameters` | a count that does not fit |
| `KAGParser.paramMacros: <name>[<i>] is not a scalar (…)` | a non-scalar element |
| `KAGParser: argument cannot be converted to a string` | an argument that is not a string, number or void |

The walk raises the reference's own scenario messages:

| message | condition |
|---|---|
| `Readed scenario file %1 is empty.` | the scenario has no lines |
| `Syntax error.'[' match to ']', " match to ", 'macro' march to 'endmacro'.      Notice space and newline.` | a syntax error (the reference's text, typos included) |
| `Label %2 not found in scenario file %1.` | `goToLabel` with an unknown label |
| `'return' is not matched to any 'call' ( 'return' is unexpected )` | a `return` with an empty call stack |
| `Lost return position due to the scenario file changed.` | a return whose frame no longer matches |
| `Label in macro 'iscript' is illegal.` | a label inside a macro recording |
| `Can not ommit first label name in scenario.` | an empty first label |
| `Matched [endscript] or @endscript not found.` | an unterminated `[iscript]` block |

The reference raises the syntax and first-label errors **lazily**, when the
walk reaches them; the port does the same, so a scenario with an error later
in the file still yields its earlier tags first.

---

## 8. `store` / `restore`

`store()` returns a Dictionary with two kinds of member:

* `data` — the emulator snapshot `restore()` reads: the whole walk state
  encoded with the `kag/kvp.rs` codec. It records the position, but
  `restore()` does **not** apply `eventIndex`, `curLine`, `curPos` or the
  active line buffer: the reference `Restore` reloads the scenario and calls
  `GoToLabel(curLabel)` instead (`reference/cpp/core/base/KAGParser.cpp:723-906`).
  A scenario saved while paused on a tag therefore runs that tag again.
* the reference's own top-level members: `storageName`, `storageShortName`,
  `curLabel`, `curLine`, `curPos`, `lineBuffer`, `lineBufferUsing`,
  `ExcludeLevel`, `IfLevel`, `ExcludeLevelStack`, `IfLevelExecutedStack`,
  `macroArgStackBase`, `macroArgStackDepth`, plus the nested `macros`,
  `macroArgs` and `callStack` containers.

`restore(dict)` accepts either shape: a dictionary with a string `data`
member is imported from that snapshot; anything else is imported as a
reference-format dictionary. Both paths restore the reference fields and
rewind to `curLabel`; the saved current position is not restored. The import
maps the reference's member names onto the emulator's (`orgLineStr` → the
call frame's original line, `pos` → the frame's cursor position) and
enforces the reference's depth check. Call-stack frames keep their own saved
positions, so `return` still resumes after its `call`.

---

## 9. Tests

`crates/compat/tests/kag.rs` compiles `parser.rs`, `state.rs` and `kvp.rs`
directly into the test binary, so the pure state machine is tested without a
VM. It covers:

* the parser's line model, tag syntax, quoting, escapes, labels,
  directives, comments, inline scripts and every syntax error;
* the multi-line tag extension (on and off);
* the walk: text/line-end tags, `curLine`/`curPos`, `ignoreCR`, the `if` /
  `elsif` / `else` / `endif` machinery, `ignore` regions, macro recording
  and expansion, `emb` splicing, `jump` / `call` / `return`, the veto
  callbacks, `interrupt`, `assign`, `clear`;
* the native surface: the tag Dictionary and its `taglist`, the read-only and
  stable `macros` property, `paramMacros` round-trip and expansion, the
  `store`/`restore` label rewind, the reference-format import, the
  `assign` rejection of a non-parser, the `debugLevel` integer conversion and
  the 0-based `curLine`.

---

## 10. Known gaps

1. **Macro recording reconstructs the body** from the parsed events
   (`[name key=value …]`, re-quoting values that would not re-parse) instead
   of copying the raw source text. The recorded body re-parses to the same
   events, which is what the expansion path needs, but the `*` macro-entity
   marker is lost in the recorded body (it survives in the expansion path).
2. **`%`-directive lines are walked as ordinary text.** The base reference
   has no `%` handling at all, so the script-visible behaviour matches.
3. **A `%` line is grouped into one `Directive` event** by the parser, and
   the walk emits its characters as text.
4. **The reference's `curLine` is 0-based** and so is this port's; the
   parser's own event line numbers are 1-based.
5. **The KAGParserEx extensions are opt-in**: `multiLineTagEnabled` is off by
   default and `paramMacros` starts empty, matching the base parser. Both are
   additions to the reference surface, not replacements.

---

## 11. Related documents

| topic | document |
|---|---|
| the L3 layer, the surface lock, the plugin model | [`../../arch/l3.md`](../../arch/l3.md) |
| the L3 ↔ L2 protocol and the state ownership rules | [`../../rules/api_rules.md`](../../rules/api_rules.md) §4, §7 |
| layer boundaries, the frame contract | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) |
| the storage namespace and the naming grammar | [`storages.md`](storages.md) |
| the script execution surface | [`scripts.md`](scripts.md) |
| the native/VM architecture and the plugin ABI | [`../../../docs.old/migration/08-tjs-vm-and-native-plugins.md`](../../../docs.old/migration/08-tjs-vm-and-native-plugins.md) |
| the L3 ↔ L2 protocol in full | [`../../../docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
