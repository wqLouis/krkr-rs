# `tjs::vm` — the script columns in Rust

Source: `crates/tjs/src/vm/` · module `tjs::vm` · L4

The TJS2 language, implemented in Rust. It is a module of the `tjs` crate,
beside the C++ tree it replaces: `tjs::vm` holds the Rust columns, and
`crates/tjs/cpp/tjs2/` holds the C++ ones they are written against.

The VM is being replaced one column at a time, and this module is where the
Rust columns live: the **lexer** (C1), the **parser** (C2) and the **code
generator** (C3). Each column has a native Rust API over one
implementation — the C ABI is a thin facade, not a second
implementation. The C ABI is panic-safe and crosses as values: a status code
plus separately retrievable detail, never a Rust type and never a panic
unwinding into C++.

## 1. Layout

| piece | where |
|---|---|
| the compile entry | `crates/tjs/src/vm/mod.rs` — the front end's own door: the script text in, the image of the script block out |
| the lexer | `crates/tjs/src/vm/lexer/` — the analyser, its token kinds and its values |
| the parser | `crates/tjs/src/vm/parser/` — the recursive-descent parser and the node tree |
| the code generator | `crates/tjs/src/vm/codegen/` — the node tree to bytecode |
| the image | `crates/tjs/src/vm/codegen/ffi.rs` and `crates/tjs/cpp/tjs2_codegen.h` — the shape the compile entry hands back, and the header the C++ side reads |

The two columns that have a page of their own:
[`parser.md`](parser.md) carries the grammar's productions, and
[`generator.md`](generator.md) carries the rules the generator
implements — each with the reference line that defines it.

## 2. The columns

The reference's own sources are the oracle for each column:
`reference/krkr2/cpp/core/tjs2/tjsLex.cpp`,
`reference/krkr2/cpp/core/tjs2/bison/tjs.y` and
`reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp`.

The reference's tree is read-only material. Each column's behaviour is
proven against it over the game's own scripts, and the proof is reported
with the measurements in [`../../perf.md`](../../perf.md).

The speed bar for a column is that it is **faster** than the C++ it
replaces, measured on the same corpus with the same harness at the same
optimisation level — not merely equal.

### The order the columns are wired in

The three columns are wired **to each other in Rust first**: the Rust lexer
drives the Rust parser, which drives the Rust generator, over native Rust
calls. No C ABI stands between them, so the node tree is never copied and
the parser's tree type is the generator's tree type.

The C ABI comes **last**, at the generator's end, and it is **one**: the
C++ VM calls one entry — compile this script text — and receives the
script block the C++ executor consumes. The C++ never calls the lexer or
the parser separately: the C++ grammar is replaced wholesale, not
adapted. Nothing else crosses the boundary: the lexer's and the
generator's own C interfaces are gone, and the image the entry hands back
is the only thing the C++ side reads.

## 3. The seam

One entry, one image. The C++ side calls something like *compile this
script text* and gets back the script block: for every context the code
array and its size, the constant area, the variable and argument counts, the
labels and the per-instruction source positions, and for the block the line
table.

The C++ side's obligations are fixed by the reference: it must construct the
same script block the reference's grammar constructs, and it must not repeat
the fix-ups the reference's generator performs after the grammar has built
the code. Those fix-ups move to the Rust generator with the rest of the
column.

The layering gate records the seam as a reviewed FFI location; see
`scripts/krkr_gates/checks/`.

The seam is specified in [`seam.md`](seam.md) — the entry, the C ABI, the
post-processing the Rust side must take over from the C++ one, and what the
C++ side must not do — with the field-by-field table of the image in
[`seam-fields.md`](seam-fields.md).

Each column has a page of its own: [`lexer.md`](lexer.md),
[`parser.md`](parser.md) and [`generator.md`](generator.md) carry the rules
that column implements, each with the reference line that defines it.

---

See also: [`../tjs.md`](../tjs.md) — the C++ VM these columns replace,
[`../../arch/l4.md`](../../arch/l4.md) — the layer, and
[`../../perf.md`](../../perf.md) — the measurements.
