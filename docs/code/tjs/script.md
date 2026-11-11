# `script` — the script language in Rust

Source: `crates/tjs/rust/script/` · crate `script` · L4

The TJS2 language, implemented in Rust. It is a workspace member that lives
inside the `tjs` crate's directory, beside the C++ VM whose columns it
replaces; `tjs` excludes it from its own package the way `media` excludes
`psd`.

The VM is being replaced one column at a time, and this crate is where the
Rust columns live: the **lexer** (C1), the **parser** (C2) and the **code
generator** (C3). Each column has a native Rust API and an `extern "C"` API
over one implementation — the C ABI is a thin facade, not a second
implementation — and the C ABI is panic-safe: a panic is caught at the
boundary and reported through the ABI, never allowed to unwind into C++.

Errors cross the C ABI as codes, with the detail retrieved separately. No
Rust type crosses the boundary.

---

## 1. Layout

| piece | where |
|---|---|
| the lexer | `src/lexer/` — the analyser, its token kinds and its values |
| the lexer's C ABI | `src/ffi.rs` and `include/tjs2_lexer.h` |
| the parser | `src/parser/` — the recursive-descent parser and the node tree |
| the code generator | `src/codegen/` — the node tree to bytecode |

## 2. The columns

The reference's own sources are the oracle for each column:
[`tjsLex.cpp`](../../reference/cpp/core/tjs2/tjsLex.cpp),
[`bison/tjs.y`](../../reference/cpp/core/tjs2/bison/tjs.y) and
[`tjsInterCodeGen.cpp`](../../reference/cpp/core/tjs2/tjsInterCodeGen.cpp).

The reference's tree is read-only material. Each column's behaviour is
proven against it over the game's own scripts, and the proof is reported
with the measurements in [`../../perf.md`](../../perf.md).

The speed bar for a column is that it is **faster** than the C++ it
replaces, measured on the same corpus with the same harness at the same
optimisation level — not merely equal.

## 3. The seam

The C++ VM calls the Rust columns through the `extern "C"` surface, so the
seam is the one place the two implementations meet. The layering gate
records it as a reviewed FFI location; see `scripts/krkr_gates/checks/`.

---

See also: [`../tjs.md`](../tjs.md) — the C++ VM these columns replace,
[`../../arch/l4.md`](../../arch/l4.md) — the layer, and
[`../../perf.md`](../../perf.md) — the measurements.
