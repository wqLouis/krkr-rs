# `compat::natives::clipboard` — the `Clipboard` class

Source: `crates/compat/src/natives/clipboard.rs` · layer **L3** · class
`Clipboard`

`Clipboard` is the script's access to the system clipboard. It is registered
as a **class-level** native class: both members are static and there is no
instance payload.

---

## 1. Registered surface

| member | kind | behaviour |
|---|---|---|
| `hasFormat(format)` | static method | whether the clipboard holds the given format |
| `asText` | static property, read/write | the clipboard's text |

There are **two** members. The module header lists a third — a `Clipboard`
constructor — but none is registered, and the surface dump agrees.

---

## 2. The host seam

The clipboard is a **host trait**, not a storage-backed thing: L3 has no
storage involvement and no file access.

```rust
pub trait ClipboardBackend {
    fn has_format(&self, format: ClipboardFormat) -> bool;
    fn text(&self) -> Option<String>;
    fn set_text(&self, text: &str);
}
```

A blanket implementation covers any tuple of three closures
(`Fn(i64)->bool`, `Fn()->Option<String>`, `Fn(String)`), so a host can
install the backend without naming the trait.

The host installs it with:

```rust
CompatHost::set_clipboard_backend<B: ClipboardBackend + 'static>(&self, backend: B);
```

`ClipboardFormat` has exactly one variant, `Text = 1`. `from_code` returns
`None` for anything else.

---

## 3. Behaviour

`hasFormat(format)` reads the format as an integer. **Only `cbfText` (1) is
a known format**; every other code returns `false` **without consulting the
backend** — the reference's `switch` default. For `cbfText` the backend is
asked.

`asText` reads the backend's text. It is **void** when the backend reports
no text — the reference returns void when the clipboard has no text data.

Writing `asText` replaces the clipboard text. The value is coerced with TJS
`ttstr`: strings pass through, integers and reals use their decimal form, and
**void and objects become the empty string**.

---

## 4. The deliberate divergence

**No backend ⇒ a typed error.** `Clipboard.hasFormat` and `Clipboard.asText`
raise `no clipboard backend is installed` rather than returning a fabricated
`false` or an empty string.

This is the one intentional behavioural difference from the reference, whose
non-platform stub returns `false` / no text. The port refuses to guess when
the host has not installed a clipboard.

---

## 5. State ownership

| state | owner |
|---|---|
| the installed backend | the **per-engine dispatch state** (`CompatServices::state::<ClipboardState>()`) |
| the clipboard contents | the **host** — L3 holds no copy |

There is no per-object payload and no L2 state.

---

## 6. Errors

| shape | example |
|---|---|
| `"Clipboard.hasFormat requires 1 argument"` | the argument count, checked **before** the backend check |
| `"Clipboard.hasFormat: compat context is not bound"` | the engine binding is missing |
| `"Clipboard.hasFormat: no clipboard backend is installed"` | no backend |
| `"Clipboard.asText: compat context is not bound"` | the engine binding is missing |
| `"Clipboard.asText: no clipboard backend is installed"` | no backend |
| `"Clipboard.asText: missing value"` | a setter without a value slot |

A non-integer `format` raises the shared conversion errors, unprefixed:
`value is null, not an integer`, `value is an object, not an integer`,
`value is an octet, not an integer`, `value cannot be converted to an
integer`.

---

## 7. Tests

`crates/compat/tests/clipboard.rs` covers the typed errors when no backend
is installed (including that the argument count is checked first), a stub
backend round-tripping the text, `asText` reading back as void when the
backend has no text, and an unknown format code returning `false` without
consulting the backend.

---

## 8. Known gaps

1. **Only text is supported.** `ClipboardFormat` has a single variant, so
   `hasFormat` can only ever be asked about `cbfText`.
2. **No backend raises** instead of returning the reference's inert values —
   deliberate, see §4.
3. **No `Clipboard` constructor member is registered**, despite the module
   header listing one.

---

## 9. Related documents

| topic | document |
|---|---|
| the L3 layer and the host seams | [`../../arch/l3.md`](../../arch/l3.md) |
| the host environment and the error contract | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §6, §7 |
