# json (`json.dll`)

**DLL name:** `json.dll` — linked with `Plugins.link("json.dll")`
**Rust module:** `crates/compat/src/natives/plugins/json/`
**Reference:** `reference/cpp/plugins/json/` (`Main.cpp`, `Writer.hpp`, the `wamsoft/json` sources)
**Registers:** four static members on the core `Scripts` class

## Surface

| member | kind | behaviour |
|---|---|---|
| `Scripts.evalJSON(text)` | static method | parses a JSON string, returning an `Array` / `Dictionary` / scalar |
| `Scripts.evalJSONStorage(name[, utf8])` | static method | reads `name` through storage and parses it |
| `Scripts.saveJSON(name, obj[, utf8[, newline]])` | static method | serializes `obj` to a JSON file |
| `Scripts.toJSONString(obj[, newline])` | static method | serializes `obj` to a JSON string |

The reference attaches these four members in `json_init` in `Main.cpp`. The
port attaches them the same way, as static members of `Scripts` — the core
`Scripts` class also carries the members from the `ScriptsEx` port; see
[`scripts_ex`](scripts_ex.md).

## Behaviour

### Value mapping

TJS2 has no boolean type: `true` / `false` are the integers `1` / `0`, and
the reference parser stores JSON `null` (and its `void` extension) as TJS
`void`. The port keeps that mapping, so a round trip through
`evalJSON` / `toJSONString` is lossless for every JSON value:

| JSON | TJS |
|---|---|
| `null` | `void` (the reference's `var.Clear()`) |
| `true` / `false` | `1` / `0` |
| number with `.` or an exponent | real |
| integer | integer |
| string | string |
| array | `Array` |
| object | `Dictionary` |

### Parsing

Parsing runs through `serde_json`: `to_strict_json` rewrites the reference's
relaxed dialect to strict JSON, and `serde_json` performs the actual parse
(values, containers, numbers, escapes, surrogate pairs and trailing-data
detection).

Input is a superset of strict JSON. `to_strict_json` accepts the reference's
extensions and rewrites them so `serde_json` sees only strict JSON with
identical parsed values:

* `#`, `//` and `/* … */` comments become one separating space;
* single-quoted strings become double-quoted (a raw `"` inside is escaped);
* `;` → `,` and `=` / `=>` → `:` as element and key separators;
* the `void` keyword → `null`; any other bare identifier is rejected with the
  typed `unknown keyword` error;
* `\xNN` escapes → `\u00NN`, with the same hex-digit validation as
  `\uNNNN`;
* number literals are completed or stripped so the value is unchanged but the
  text is strict (`+5` → `5`, `007` → `7`, `.5` → `0.5`, `1.` → `1.0`).

The rewrite is deliberately lexer-scale, not a parser: it tracks string and
comment context so a rewrite never touches a string literal, and leaves every
structural decision to `serde_json`.

### Writing

`JsonWriter` reproduces `IWriter` / `getVariantString` in `Writer.hpp` and
`Main.cpp` byte-for-byte for finite values:

* one space of indentation per nesting level;
* a newline inside every container and after the last member;
* the newline is CRLF when the `newline` argument is `0` (the default) and LF
  otherwise;
* members are written as `"key":value`.

Any non-`Array` object is serialized as a dictionary, exactly like the
reference.

The reference walks a dictionary with `iTJSDispatch2::EnumMembers`, which the
Rust ABI does not expose. The serializer instead asks the builtin
`Array.assign` to copy the dictionary's own data members into an array as
`[key0, value0, key1, value1, …]`; `Array.assign` performs exactly the
reference's `EnumMembers` walk, so the member order and the hidden-member
filtering match.

### File encoding

`saveJSON` writes the serialized text through the attached storage. The
`utf8` argument selects the encoding: with `utf8` set the text is written as
UTF-8; without it the reference writes the current code page, and the port
names CP932 for that case (`anaul.tjs:16,19`) — detecting the code page
would read CP932 `"あ"` as windows-1251.

`evalJSONStorage` reads the file the same way: with `utf8` set the bytes are
decoded as UTF-8, otherwise as CP932.

## State and lifetime

The plugin keeps no per-object state — the four members are static and hold
no state between calls.

One TJS helper is installed once per engine: the name of a TJS object that
distinguishes TJS `null` from other objects. The safe engine API collapses
`null` and a real object into `TjsValue::Object`, so the serializer asks the
VM directly. The ABI installs every plugin helper as a global, but the
reference registers no free function here
(`reference/cpp/plugins/json/jsonPlugin.cpp:98-103` attaches only the four
`Scripts` members), so `register` defines the helper, retains it and removes
the global binding.

## Errors

| condition | outcome |
|---|---|
| a structural mistake in the JSON | `serde_json`'s message, prefixed with the member name |
| a lexer-level failure (bad escape, unknown keyword, malformed number) | the plugin's own typed text — `unexpected end of input at byte N`, `invalid number at byte N`, `invalid escape at byte N`, `invalid unicode escape at byte N`, `unknown keyword "x" at byte N` |
| input deeper than `serde_json`'s recursion limit | a typed error |
| a non-finite real (TJS `NaN` / `Infinity`) | typed error: `real value N is not finite and cannot be represented in JSON` |
| a member is called without its required argument | typed error: `<member> requires 1 argument` |
| the storage is not attached, or the read fails | the storage's error, prefixed with the member name |

## Tests

| file | what it pins |
|---|---|
| `crates/compat/tests/json.rs` | the value mapping for every JSON type, the relaxed-dialect extensions, the parse and writer error messages, the writer's byte-exact layout (indentation, newlines, `CRLF` vs `LF`), the dictionary enumeration order, the storage round trip, and the non-finite real error |

## Known gaps

* The reference's "current code page" is a per-machine setting. The port
  names CP932 for it, which is the code page the reference's own scripts
  assume (`anaul.tjs:16,19`).
* The reference walks a dictionary with `EnumMembers`; the port reproduces
  that walk through the builtin `Array.assign`, which visits the same
  members in the same order and applies the same hidden-member filtering.
* A real whose value is not finite has no JSON representation; the port
  raises a typed error where the reference would emit a non-standard token.
