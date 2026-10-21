# json (`json.dll`)

**DLL name:** `json.dll` — linked with `Plugins.link("json.dll")`
**Rust module:** `crates/compat/src/natives/plugins/json/`
**Reference:** `reference/cpp/plugins/json/jsonPlugin.cpp` (the four members) and `reference/cpp/plugins/json/anaul.tjs` (the plugin's own manual)
**Registers:** four static members on the core `Scripts` class

The members appear on the first `Plugins.link("json.dll")`; the built-in table
and the linking rules are on the [plugin index](../plugins.md).

## Surface

| member | kind | behaviour |
|---|---|---|
| `Scripts.evalJSON(text)` | static method | parses a JSON string, returning an `Array` / `Dictionary` / scalar |
| `Scripts.evalJSONStorage(name[, utf8])` | static method | reads `name` through the storage and parses it |
| `Scripts.saveJSON(name, obj[, utf8[, newline]])` | static method | serialises `obj` to a JSON file; returns `1` |
| `Scripts.toJSONString(obj[, newline])` | static method | serialises `obj` to a JSON string |

The four members land on the `Scripts` class object and not on its instances,
so they are reached as `Scripts.<name>` and never as free globals
(`reference/cpp/plugins/json/jsonPlugin.cpp:98-102`). The core class is
described in [`compat::natives::scripts`](../scripts.md); the other plugin
that extends it is [`scripts_ex`](scripts_ex.md).

## Behaviour

### Value mapping

TJS2 has no boolean type: `true` and `false` are the integers `1` and `0`,
and a JSON `null` becomes TJS `void`. A round trip through `evalJSON` and
`toJSONString` is therefore lossless for every JSON value.

| JSON | TJS |
|---|---|
| `null` | `void` |
| `true` / `false` | `1` / `0` |
| an integer | integer |
| a number with a fraction or an exponent | real |
| a string | string |
| an array | `Array` |
| an object | `Dictionary` |

An integer that does not fit the 64-bit range becomes a real; an overflowing
exponent such as `1e400` saturates to ±infinity. A JSON array element that
is `null` is stored as `void` in the array, so the later elements keep their
positions.

### Parsing

`evalJSON` converts its argument to text with the plain string conversion, so
a number is parsed as the number it spells.

The parse itself runs through `serde_json`: a normalisation pass rewrites the
relaxed dialect to strict JSON, and `serde_json` performs the actual parse
(values, containers, numbers, escapes, surrogate pairs and trailing-data
detection). The normalisation pass is deliberately lexer-scale: it tracks the
string and comment context, so a rewrite never touches a string literal, and
it leaves every structural decision to `serde_json`.

The relaxed dialect it accepts:

| input | rewritten as |
|---|---|
| a `#`, `//` or `/* … */` comment | one separating space |
| a single-quoted string | a double-quoted string (a raw `"` inside is escaped) |
| `;` | `,` — the element separator |
| `=` or `=>` | `:` — the key separator |
| the `void` keyword | `null` |
| `\xNN` | `\u00NN`, with the same hex-digit validation as `\uNNNN` |
| `+5`, `007`, `.5`, `1.` | `5`, `7`, `0.5`, `1.0` — the value is unchanged, the text is strict |

Any other bare identifier is rejected: only `true`, `false`, `null` and
`void` exist.

### Writing

`toJSONString` and `saveJSON` share one writer. Its layout is fixed:

* the `newline` argument selects the line ending: `0` (the default) is a
  carriage return followed by a line feed, anything else is a line feed;
* one space of indentation per nesting level;
* a newline after the opening bracket and before the closing bracket, and one
  between the members;
* the members of a dictionary are written as `"key":value`, comma-separated.

So `toJSONString([1,2,3])` writes `[`, a newline, ` 1,`, a newline, ` 2,`, a
newline, ` 3`, a newline and `]` — the newlines being CRLF by default. An
empty container still carries the two newlines: `[`, a newline, one space
(the indent of the level it would have used), a newline and `]`.

The values are written as:

| value | written as |
|---|---|
| `void` and TJS `null` | `null` |
| an integer | its decimal form |
| a real | the shortest form that reads back as the same value (`100`, `3.5`) |
| a string | a double-quoted literal, with `"`, `\`, `\b`, `\f`, `\n`, `\r` and `\t` escaped and every other control character as `\u00xx` |
| an `Array` instance | an array |
| any other object | a dictionary of its own data members |
| an octet | `null` — an octet is not part of the JSON value model |

An `Array` is recognised through the object's class-name chain, so a script
subclass of `Array` is written as an array. Every other object is written as
a dictionary, exactly like the reference.

The dictionary walk needs the object's own data members in enumeration order.
The compat ABI does not expose the reference's `EnumMembers`, so the writer
asks the builtin `Array.assign` to copy the object into an array as
`[key0, value0, key1, value1, …]`; `Array.assign` performs the same walk, so
the member order and the hidden-member filtering match. The class members the
ABI attaches to the object are not data members and never appear in the
output.

### Files

`evalJSONStorage(name[, utf8])` reads the file through the attached storage —
or the host filesystem when no storage is attached — and parses it. The name
goes through the storage-name boundary first, so a `file://` prefixed name is
accepted. The `utf8` argument selects the encoding: non-zero means UTF-8,
zero means the current code page, which the port names CP932.

`saveJSON(name, obj[, utf8[, newline]])` serialises the object and writes it.
The name is resolved the way the reference resolves it before writing
(`reference/cpp/plugins/json/jsonPlugin.cpp:62`): an absolute path — or a
Windows drive path — is used as it is, and a relative one is joined onto the
game directory of the attached storage. The parent directories are created if
they are missing, and the bytes land on the host filesystem next to the game
data; the write does not go into a mounted archive. The encoding follows the
`utf8` argument: UTF-8, or CP932 for the current code page.

`saveJSON` always returns `1` — TJS's `true`. The reference's `false` in the
catch branch is dead, because the `true` assignment runs unconditionally
afterwards (`reference/cpp/plugins/json/jsonPlugin.cpp:69-76`).

## State and lifetime

The four members are static and hold no state between calls.

One TJS helper is installed once per engine: a function that tells TJS
`null` from every other object, because the safe engine API reports `null` as
an object. The registration defines it as a script global so the VM compiles
it, retains it and then removes the global binding — the reference registers
no free function for the plugin
(`reference/cpp/plugins/json/jsonPlugin.cpp:98-102`). The retained id lives
in the per-engine dispatch state and dies with the VM.

## Errors

| condition | outcome |
|---|---|
| `evalJSON` is called without its argument | typed error: `Scripts.evalJSON requires 1 argument` |
| `evalJSONStorage` is called without its argument | typed error: `Scripts.evalJSONStorage requires 1 argument` |
| `saveJSON` is called with fewer than two arguments | typed error: `Scripts.saveJSON requires 2 arguments` |
| `toJSONString` is called without its argument | typed error: `Scripts.toJSONString requires 1 argument` |
| a structural mistake in the JSON | `serde_json`'s message, prefixed with the member name |
| a lexer-level failure | typed error: `unexpected end of input at byte N`, `invalid number at byte N`, `invalid escape at byte N`, `invalid unicode escape at byte N`, `unknown keyword "x" at byte N` |
| a real whose value is not finite | typed error: `real value N is not finite and cannot be represented in JSON` |
| the file cannot be read | typed error: `Scripts.evalJSONStorage: '<name>': …` |
| the file cannot be written | typed error: `Scripts.saveJSON: …` |
| the text cannot be encoded in the current code page | typed error: `the text cannot be encoded in the current code page` |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/json.rs` | the value mapping for every JSON type, the number typing split at the integer/real boundary, the relaxed-dialect extensions, the surrogate-pair decoding, the parse error messages, the writer's byte-exact layout (indentation, the newlines, CRLF vs LF, the string escapes, the empty container), the dictionary enumeration order against a hand-built dictionary, the octet fallback, the non-finite real error, the class-only placement of the four members, the absence of the helper as a global, the storage round trip, the CP932 encoding in both directions, the relative-path resolution and the `true` result of `saveJSON` |

## Known gaps

* The reference in the tree is a **stub**. `reference/cpp/plugins/json/jsonPlugin.cpp`
  checks the arity of the four callbacks and then returns its input verbatim;
  each callback carries a remark that it should really parse or stringify
  (`:22-25`, `:39-52`, `:67-68`, `:89-91`). The behavioural spec that is in the
  tree is the plugin's own manual, `reference/cpp/plugins/json/anaul.tjs` — the
  signatures, the defaults, the encodings and the newline codes — plus what
  the stub itself observably does: the argument counts, the `utf8` and
  `newline` defaults, `saveJSON`'s `true` result and the `void` result of a
  failed read. The writer's exact byte layout and the relaxed-dialect grammar
  therefore have no in-tree implementation to be compared against.
* Where the stub and the port disagree, the port follows the manual:
  `evalJSON` returns the parsed value instead of the argument unchanged
  (`:22-25`); `evalJSONStorage` parses the file instead of returning its text,
  and raises a typed error for a file it cannot read instead of swallowing the
  failure into a `void` result (`:39-53`); `toJSONString` writes JSON text
  instead of the argument's string form (`:89-91`); `saveJSON` writes the
  serialised JSON instead of the argument's string form (`:67-70`). The
  arity checks, the defaults and the `true` result are the same.
* The reference's "current code page" is a per-machine setting. The port
  names CP932 for it — the code page the reference's own manual assumes
  (`reference/cpp/plugins/json/anaul.tjs:16,19`).
* The reference walks a dictionary with `iTJSDispatch2::EnumMembers`, which the
  compat ABI does not expose. The port reproduces that walk through the
  builtin `Array.assign`, which visits the same members in the same order and
  applies the same hidden-member filtering.
* A real whose value is not finite has no JSON representation. The port
  raises a typed error, where the reference's writer would emit a
  non-standard token.
* `saveJSON` writes to the host filesystem — the game directory for a relative
  name — and not into a mounted archive.
