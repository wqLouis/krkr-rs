# `tjs::vm::runtime` — the runtime in Rust

Source: `crates/tjs/src/vm/runtime/` · module `tjs::vm::runtime` · L4

The runtime half of the script VM: the values a script manipulates, the
objects that hold its members, and the contexts that execute its code. The
front end beside it — the lexer, the parser and the code generator — turns
script text into intermediate code; this module is the machine that runs it.

The runtime is built **bottom up, one part per change**, and each part is
deleted from the vendored tree in the change that takes it over. What is here
today is the **value layer**; the object model, the variant, the interpreter
and the builtin natives follow, in the order `TODO.md` §12.1 records.

The text-to-number rule has already been taken over: `tjsNumberParse.cpp` and
the `TJS_strtod` it exported are deleted, and the five place the C++ side
converted text reads this module through one C entry. Everything else of the
vendored runtime still runs, met at the one compile seam
(`docs/code/tjs/seam.md`); a part lands here proven first, and only then is the
code it replaces removed.

---

## 1. Layout

| piece | where | what it is |
|---|---|---|
| the string | `runtime/string.rs` | `TjsString`: an immutable sequence of UTF-16 code units, cheap to clone |
| the number | `runtime/number.rs` | `Number` (an integer or a real), the text-to-number rules, and the number-to-text form |
| the member table | `runtime/hash.rs` | the keyed table a dictionary, a message map and the script cache are stored in |
| the member map | `runtime/member.rs` | an object's own symbol table: a power-of-two array whose enumeration order is script-visible |
| the variant | `runtime/variant.rs` | the tagged value, the closure handle and the coercions |
| the octet | `runtime/octet.rs` | the byte-sequence value |
| the object | `runtime/object.rs` | the custom object, its member records and the dispatch a member access goes through |
| the measurement | `runtime/bench.rs` | the A/B harness against the vendored primitives; `#[cfg(test)]`, `#[ignore]`d |

The module denies `unsafe`: a value owns its storage and is borrowed or
cloned, and a part that cannot be written that way is a part whose shape is
wrong.

---

## 2. The value layer

### 2.1 The string

`TjsString` is a sequence of UTF-16 code units — not bytes and not
characters — and its length is the number of units
(`reference/krkr2/cpp/core/tjs2/tjsVariantString.cpp:500-504`). A valid
surrogate pair is two units; a lone surrogate is a unit like any other.
Construction cuts the text at the first zero code unit, because the buffer
behind the engine's own string is zero-terminated and re-measures itself
with `strlen` (`:507-516`).

| rule | where the reference defines it |
|---|---|
| the length is code units | `tjsVariantString.cpp:500-504` |
| the text ends at the first zero unit | `tjsVariantString.cpp:507-516` |
| comparison is by code unit, no case folding | `tjsConfig.cpp:551-566`, `tjsString.h:167-176` |
| the member hash: one-at-a-time over the units, `0xffffffff` when the mix lands on zero, `0` for the empty text | `tjsHashSearch.h:57-78` |
| concatenation appends units | `tjsString.h:262-273` |
| `escape_c`: the named escapes, `\xNN` for a control unit, and the flag that keeps a following hex digit escaped | `tjsString.cpp:160-229` |
| `unescape_c` answers empty | `tjsString.cpp:232-235` |
| the coercions `to_number`/`to_integer`/`to_real` | `tjsVariantString.cpp:481-493`, `:457-478` |

A text is built in **one allocation**: an ASCII text — every member name the
engine builds — fills an `Arc<[u16]>` in place, a text that is not stays at
two, and a concatenation is one because the chain of two slice iterators is
trusted-length. That matters because a member name is built on every lookup
by the front end and by the compatibility surface.

### 2.2 The number

`Number` is the two numeric kinds the language has, an integer (64-bit) and
a real. `parse` reads a number from the front of a text and answers it with
the extent it occupied; the rules are the engine's: the sign and the spaces
after it, the `0`/`0x`/`0X`/`0b`/`0o` prefixes, the exponent form, the
nine-digit fast path, and the slow path's 48-digit mantissa cap and 511
exponent cap (`tjsConfig.cpp:672-840`, `tjsLex.cpp:382-429`, `:611-697`).

A text that is not a number is a **typed error**, not a zero: the zero is
the rule of the arithmetic that coerces a string, and it lands where the
coercion happens (`tjsVariantString.cpp:481-492`, `dest = 0`), never in the
conversion. The distinction is not observable to a script — no member
reports it and `typeof` of either result is the same — so the runtime may
keep it internally, and the variant must turn `NotANumber` into an
**integer** zero, which is what the engine does.

Writing a real is the `{:.15g}` form with the engine's own special cases
written out (`tjsVariant.cpp:213-249`): `NaN`, `+Infinity`/`-Infinity`, and
`+0.0`/`-0.0` with their signs, then fifteen significant digits, the
exponent form below `1e-4` or at `1e15` and above, and the insignificant
zero digits dropped.

### 2.3 The member table

`HashTable<K, V>` files a key into 64 buckets by the key's own 32-bit hash,
chains an overflow entry, and threads an **order list** through the entries so
enumeration follows the order they were last written in
(`tjsHashSearch.h:20`, `:243`, `:507-518`, `:542-555`, `:530-539`): a write
to a present key moves it to the front, a removal closes the gap wherever it
stands, a re-insertion is a new entry at the front, and `chop_last` takes the
oldest — the operation the cache class in the same header needs.

Only that order is observable. The bucket count, the probe sequence and the
hash values are not, so the table is free to be an arena with `u32` links, a
chain predecessor for a constant-time unlink, and a swap-remove that keeps the
arena dense — which is what makes a chain step one array read and one compare.

A key supplies its own hash through `TableKey`, so a string is filed under the
member hash above and nothing in the table needs to know what a string is.

**The fixed bucket count is the original's, not a policy.** The table does not
grow or rehash, because the original does not, and a structural change nobody
measured is not part of a port.

### 2.4 The member map is not this table

`tTJSCustomObject` does not store its members in `tTJSHashTable`: it keeps a
power-of-two symbol array with its own `RebuildHash` (`tjsObject.cpp:325-340`,
`:649-756`) and enumerates it in **bucket order** (`:1094-1127`), which is the
order a dictionary's structured save follows (`tjsDictionary.cpp:433`,
`:499`, `:508`). This table enumerates newest-first insertion order, which is
what the message map and the script cache rely on (`tjsMessage.cpp:64`).

So the object model ports its member map from the object itself, and this table
serves the message map, the script cache and the values map (`tjs.cpp:88`).
Putting members on this table would silently change the order `for (name in
obj)` walks and the order `Dictionary.saveStruct` writes.

---

### 2.5 The variant

The tagged value is a string, an integer, a real, a closure handle, an octet,
void, or a reference to a dispatch object. Its coercions are the engine's own,
proven against 69 recorded answers: `"a" + 1` is the string `"a1"`; `"abc" * 1`
is the **integer** zero, not a real, which is the answer a reader is most
likely to get wrong; `7 / 2` is the real 3.5 while `7 \ 2` is the integer 3;
`null == void` is false while `void == ""` is true.

Two shapes matter beyond the arithmetic. An object coerced to a string yields
its pointer form and nothing else, because the engine has no other answer
without a class's own `toString`. And an object coerced to a number, an
integer or a real raises rather than guessing: three recorded probes agree, so
the conversion error is a typed error here too. The same is true of the
default-value protocol the vendored header declares — nothing in the engine
calls it, so nothing here implements it.

The octet is the byte-sequence member of the variant. The octet *object* — its
index space and its streams — belongs to the containers, and is not here yet.

### 2.6 The object

`runtime/object.rs` is the custom object: a member map, a record that is either
a value or a **property** with a getter and a setter, and the dispatch surface a
script's member access goes through — read, write, delete, enumerate,
`is_instance_of`, `class_names`, validity. The surface is `&self` and the map
sits behind a cell, because a member read may change the table: a chain member
more than two deep moves to the head of its chain, which is the behaviour the
member map already reproduces.

The recorded answers pin three member states apart, and it took the engine to
settle them: a member that is present and void reads `void`, a member that is
absent reads `undefined`, a member holding a value reports its kind. A write
creates the member; a delete makes it absent again; reading a missing member
with the ensure flag creates it as void. The flags carry the rest: must-exist
answers an error instead of creating, ignore-property skips the getter and the
setter, hidden and static are reported by the enumeration, and enum-no-value
retrieves nothing and calls no getter.

What is deliberately absent: the call mechanics. A call is the interpreter's,
not an object method, and a trait method nothing implements is dead API. A
property whose call is not served falls back to the member's value, which is
the engine's rule for a getter that exists but does not answer.

## 3. The two proofs

A part is done on two measurements, and neither is its own test suite.

**Behaviour**, for everything a script can observe, is proven against the
engine itself. `crates/tjs/examples/value_oracle.rs` evaluates a fixed probe
table through `Tjs2Engine` and writes the answers to
`crates/tjs/tests/fixtures/value_layer.txt`; `crates/tjs/tests/value_layer.rs`
reproduces every answer with this module and fails on any difference. The
record is committed, so the check needs no engine:

```sh
cargo run -p tjs --example value_oracle \
    > crates/tjs/tests/fixtures/value_layer.txt     # record
cargo test -p tjs --test value_layer                # check
```

The check fails when the record holds only its header, and it asserts the record
holds exactly its own probe list, so a stale or truncated record cannot pass.
It is the corpus digest's idea applied to the runtime: what it records is the
behaviour of the code that is about to be deleted, and it outlives it.

The probes are the ones a script can see — a real and an integer written as
text, text read as a number in each of its forms, the kind a conversion
produces, the escape rule, the code-unit length, the comparison. One
limitation is stated rather than hidden: the probes' **literals** are compiled
by the Rust front end, so a probe whose answer is the literal's own value is a
round trip through this tree rather than an independent reading. Every other
answer is the engine's.

**A cutover** is proven the same way, one step further: while both sides
existed, the vendored rule and this module's rule were compared over 1 047
written texts and 300 000 generated ones on kind, value and extent, with **zero
divergences**, and that comparison was recorded as
`crates/tjs/tests/fixtures/number_parse.txt` before the vendored rule was
deleted. `cargo test -p tjs --test number_parse` replays the record with no C++
and no engine, so the deletion of the code the record was taken from costs the
proof nothing.

**Speed** is proven against the primitives the module replaces, with the A/B
harness in `runtime/bench.rs`: the same operations, the same inputs, both sides
at `-O2`, a checksum that the compiler cannot see through, and a warm-up pass
discarded before three measured ones. The figures, the method and the fairness
caveats are `docs/perf.md` §6.5; the command is

```sh
CARGO_PROFILE_RELEASE_OPT_LEVEL=2 \
    cargo test -p tjs --release --lib vm::runtime::bench -- --ignored --nocapture
```

---

## 4. What the layer does not do yet

The containers (the array, the dictionary, the octet object and its streams),
the interpreter, and the builtin natives. The order, and what each part
deletes, is `TODO.md` §12.1.

The vendored bodies of the variant and the object **are still in the tree** and
die with the seam, not with the Rust half: their types cross the C interface by
value — a variant as a C struct, an object as a pointer — so the vendored side
cannot call the Rust one until that interface itself is replaced. The one rule
cut over the other way is the text-to-number rule, because it is a pure
function; its 694 lines are already gone. The remaining count and the file list
live in `TODO.md` §12.1 rather than here, so that a deletion has one place to
be recorded.
