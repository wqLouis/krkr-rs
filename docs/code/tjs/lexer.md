# `tjs::vm::lexer` — the lexical analyser

Source: `crates/tjs/src/vm/lexer/` · module `tjs::vm::lexer` · L4

The analyser reads a script as a sequence of UTF-16 code units and produces
the token stream the parser consumes. A token is a kind and, for the kinds
that carry one, a literal value. The source positions it reports are counted
in code units from the start of the script, so a position names the same
place in the text the parser and the error messages name.

The analyser is driven by the parser: two notifications tell it what the
parser expects next — a regular expression literal, or a name that is not a
reserved word. Everything else it decides on its own.

## 1. The rules it implements

| rule | where the reference defines it |
|---|---|
| the analyser itself: the token kinds, the scanning and the positions | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:992-1890` |
| the reserved words | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:892-990` |
| the digit helpers and the number parser | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:89-131`, `:382-733` |
| the string scanner | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:367` |
| the octet and regular-expression scanners | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:734`, `:822` |
| a position is `Current - Script` in code units | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1656` |
| `tjs_char` is UTF-16 | `reference/krkr2/cpp/core/tjs2/tjsTypes.h:44` |
| the invalid character's message | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1499`, `:1511` |
| the escaping rule for an invalid character | `reference/krkr2/cpp/core/tjs2/tjsString.cpp:160` |
| a regular expression whose closing slash ends the script | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:846-854` |
| the notification for a regular expression, and the one for a bare word | `reference/krkr2/cpp/core/tjs2/bison/tjs.y:681`, `:688`, `:710`, `:713` |
| the flag for a conditional-compilation statement | `reference/krkr2/cpp/core/tjs2/tjsLex.cpp:1132`, `:1174` |
| that flag makes the script block non-reusable | `reference/krkr2/cpp/core/tjs2/tjsScriptBlock.h:79-82` |

The escaping rule carries a flag: a control character with no named escape
sets it and is written `\xNN`, the named escapes clear it, and while the
flag is set a hex digit is written `\xNN` too; the text ends at the first
NUL. The analyser escapes one code unit at a time, so a message never holds
two characters — the flag's branch is unreachable there, and the rule is kept
whole because the rule is the reference's.

## 2. Its proof

The analyser's tokens, values and positions are compared against the
reference's own analyser over the game's scripts, and the two agree on every
script; the measurements are in [`../../perf.md`](../../perf.md) §6.1.

## 3. Its C interface

`crates/tjs/src/vm/lexer/ffi.rs` and `crates/tjs/cpp/tjs2_lexer.h`. It is
a proof instrument, not the shipped interface: the shipped one is the
compile entry at the end of the pipeline. See
[`columns.md`](columns.md) §3.

---

See also: [`parser.md`](parser.md), [`generator.md`](generator.md) — the
other two columns, [`seam.md`](seam.md) — the seam, and
[`../../perf.md`](../../perf.md) — the measurements.
