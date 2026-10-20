# `compat::natives::tjsnum` — the TJS string-to-number coercion

Source: `crates/compat/src/natives/tjsnum.rs` · layer **L3** · **not a script
class**

This module is not a native class and registers nothing with the VM: it
appears nowhere in the surface dump. It is the Rust implementation of one
TJS2 coercion — the `string → integer` conversion — and it is shared by every
native that has to interpret a script string as a number.

It exists as its own module because the KAG state machine includes it
directly (the KAG tests compile the parser and the state machine standalone),
so it must stay free of imports beyond `std`.

---

## 1. The API

```rust
pub(crate) fn tjs_string_to_integer(s: &str) -> i64;
pub(crate) fn parse_radix(s: &str, radix: u32) -> i64;
```

`parse_radix` is shared with the real-number parser in
[`system.md`](system.md)'s module (`natives::system::tjs_string_to_real`),
which is a separate implementation with the same grammar that keeps the
fractional part.

---

## 2. The grammar

`tjs_string_to_integer` parses the **leading** number token and ignores
trailing garbage: `"12abc"` is `12`, not an error.

In order:

1. an optional sign (`+` / `-`), after which **spaces are skipped**;
2. the keywords, **case-sensitive**, and not followed by an alphabetic
   character or `_`:
   * `true` → `1`
   * `false` → `0`
   * `NaN` → the integer-indefinite value
   * `Infinity` → the integer-indefinite value;
3. `0x` / `0X` hexadecimal;
4. `0b` / `0B` binary;
5. a leading zero that is none of `0x`, `0b`, `0.` or `0e` is **octal** (a
   `0p`/`0P` exponent yields `0`);
6. otherwise a decimal integer or real.

A decimal token may have at most one `.` and at most one `e`/`E` exponent,
with optional spaces and a sign after the exponent marker. A token with a
point or an exponent is real and truncates **towards zero**.

`parse_radix` accumulates with **wrapping** 64-bit arithmetic, matching the
reference's lexer, and returns `0` when no digit parses.

---

## 3. Why it is script-observable

The module registers nothing, but its behaviour is what a script sees
whenever a string is used as a number:

* `value_as_bool` — a string is truthy iff its integer value is non-zero. So
  `"0"` is false, `"abc"` is false, `"12abc"` is true, `"true"` is true.
* `value_as_i64` — the integer value of a script string.
* `tjs_string_to_real` in `natives::system` — the same grammar with the
  fractional part kept: `"12.5abc"` is `12.5`.
* **KAG attribute values** — `emb escape=0.5` is false, because `"0.5"` has
  the integer value `0`.

---

## 4. Callers

| caller | use |
|---|---|
| `natives::system` | re-exports `tjs_string_to_integer`; `value_as_bool` and `value_as_i64` use it, and `tjs_string_to_real` shares `parse_radix` |
| `natives::kag::state` | `parse_tjs_bool` — the `escape` attribute's truthiness |
| `natives::visual::layer::transitions` | the transition parameters |
| `natives::visual::window` | a window parameter |
| the plugins | `win32_dialog`, `text_render`, `get_sample`, `window_ex` |

---

## 5. Known gaps

1. **`NaN` and `Infinity` map to the integer-indefinite value.** The
   reference's `(tTVInteger)` cast of those reals is platform-dependent; the
   port pins the x86 integer-indefinite value.
2. **A dangling `e` exponent** (`"1e"`) falls back to the mantissa prefix,
   matching `strtod`'s behaviour in the reference's decimal-real parser.
3. **The module's citations use the short `tjs2/…` form** rather than the
   full `reference/cpp/…` path the rest of the tree uses.

---

## 6. Related documents

| topic | document |
|---|---|
| the shared converters and the `System` class | [`system.md`](system.md) |
| the KAG attribute resolution that uses this coercion | [`kag/state.md`](kag/state.md) |
| the L3 layer and the script-visible surface | [`../../arch/l3.md`](../../arch/l3.md) |
