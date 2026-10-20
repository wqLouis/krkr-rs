# `compat::natives::kag::kvp` — the string codec

Source: `crates/compat/src/natives/kag/kvp.rs` · layer **L3** · no script
class of its own

Two pieces of the parser's state are naturally dictionaries but have to cross
the C ABI as strings:

* the `store()` snapshot (the whole walk state), and
* the macro bodies in the macro table.

This module is the codec for both. It is deliberately tiny and has no
dependencies.

The native's **object-shaped** results — `getNextTag()`, `macros`,
`macroParams` — are real TJS Dictionaries and do **not** use this codec.

---

## 1. The line format

A dictionary becomes one line per entry: `key=value`, with the lines joined
by `\n`.

Keys never contain a raw `=` (a KAG attribute name cannot contain one, and
the structural keys are fixed), so a decoder splits each line on its **first**
`=`.

```
version=1
storageName=first.ks
macros.foo=[bar value]
```

---

## 2. Field escaping

`escape_field` / `unescape_field` make the format lossless for arbitrary
text:

| input | escaped |
|---|---|
| `\` | `\\` |
| newline | `\n` |
| CR | `\r` |

Scenario tag content never contains a raw newline (the parser splits lines
first), but macro bodies and saved state can, so escaping is applied
everywhere for uniformity.

`unescape_field` is tolerant: an unknown escape (`` `\x ``) and a trailing
backslash are both kept **as-is** rather than rejected, so a truncated
snapshot decodes to something instead of failing.

---

## 3. API

```rust
pub fn escape_field(s: &str) -> String;
pub fn unescape_field(s: &str) -> String;
pub fn encode_dict(entries: &[(String, String)]) -> String;
pub fn decode_dict(s: &str) -> Vec<(String, String)>;
```

`encode_dict` / `decode_dict` preserve the **order** of the entries. The
order matters: the macro-argument levels and the call-stack frames are
ordered structures, and the reference's own encodings (`StoreIntStackToDic`,
`StoreBoolStackToDic`) are order-sensitive too.

`decode_dict` **skips** a line with no `=` instead of failing. A malformed
snapshot therefore decodes to the entries it does contain rather than raising.

---

## 4. Where it is used

| user | shape |
|---|---|
| `KagParserState::store` | the ordered `(key, value)` list of the whole walk state, encoded with `encode_dict` |
| `KagParserState::restore` | decoded with `decode_dict` |
| the macro table | the macro body is stored as a value in the same encoding |

The stack encodings (`encode_int_stack` / `encode_bool_stack`) live in
[`state.rs`](state.md), next to the state they encode.

---

## 5. Tests

Covered from `crates/compat/tests/kag.rs` through the `store`/`restore` round
trip, which exercises the escaping of a macro body containing a backslash and
the macro-argument levels.
