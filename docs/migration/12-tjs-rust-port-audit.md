# TJS2 engine: C++ → Rust port audit

Status: **audit + feasibility, no code changed.** Question: can we reimplement
the TJS2 scripting VM (currently the vendored C++ in `crates/tjs/cpp/tjs2`)
in pure Rust, so the build collapses to `cargo build` and the C/C++/bison/zig
"dev deps" disappear?

## 0. Is TJS just JavaScript? (Could we use QuickJS?)

No — TJS2 is a **C/Java/JS-family embedded scripting language with its own
semantics**, not JavaScript. It *looks* familiar (`var`, `function`,
`if/for/while/switch`, `try/catch/finally`, `this`, `new`, `class … extends …
super`, object/array literals, `typeof`, `instanceof`, `in`, `with`, `delete`),
which is why it reads like a QuickJS-style embedded VM — but the grammar and
runtime diverge enough that **no JS engine (QuickJS, Boa, rquickjs, V8) can run
it**.

TJS-only reserved words (from `tjsLex.cpp`): `int`, `real`, `string`, `octet`,
`getter`, `setter`, `property`, `global`, `incontextof`, `invalidate`, `isvalid`,
`synchronized`, `goto`, `enum`, `public`/`private`/`protected`/`static`, and
`NaN`/`Infinity` as keywords.

Notably **absent vs JS**: `let`, `of`, arrow functions, template strings,
destructuring, spread, generators / `async`/`await`, and `undefined`.

Semantic differences that would break real scripts:
- an explicit **int/real** numeric split (integer division, overflow, bit ops —
  JS has only doubles);
- the **`octet`** binary-buffer type;
- explicit **`getter`/`setter`/`property`** declarations and Java-style class
  access modifiers;
- **`incontextof`** (this-rebinding call), **`isvalid`/`invalidate`**
  object-lifetime calls, **`global`**, **`synchronized`**;
- a different **`typeof`** result set and a different **stdlib** (`Array`,
  `Dictionary`, `RegExp`, `Date`, `Math`, `Octet`, …) with a **`delegate`**
  mechanism.

Consequences:
- **Drop-in JS engine:** impossible — wrong language.
- **Transpile TJS2 → JS to run on QuickJS:** needs a full TJS2 front-end *plus*
  emulation of TJS semantics (int/real, octet, classes, delegates) and a stdlib
  shim — strictly more work and lossier than porting the VM, and QuickJS is C
  (another C dependency, the opposite of the goal).
- The resemblance **helps** the Rust port: the parser is a familiar C-family
  recursive-descent/Pratt job and the VM is a register machine, so the syntax is
  not the risk — regex/string/class semantics are (§4).

## 1. What we build today (dependency inventory)

`crates/tjs` compiles ~**41 k lines of C++** (37 `.cpp` + 39 `.h`) plus three
**bison** grammars, and links a C **oniguruma** regex library. `build.rs`
(900+ lines) orchestrates the whole thing. The tool/deps pulled in *only* to
produce the VM:

| Dependency | Why | Removable by a Rust port |
|---|---|---|
| **zig** (`zig c++` + package manager) | C++ compiler + fetches deps | ✅ yes |
| **bison** | generates `tjs.y` / `tjsdate.y` / `tjspp.y` parsers | ✅ yes |
| **python3** | `create_world_map.py` → `tjsDateWordMap.inc` | ✅ yes |
| **oniguruma** (C, ~50 sources) | TJS `RegExp` | ⚠️ replace with a Rust regex engine (parity risk) |
| **fmt / spdlog / boost** (header C++) | string formatting/logging in the vendored sources | ✅ yes |
| **libc++ / libc++abi** | C++ runtime at link | ✅ yes |
| C++ compiler + arch guard | everything above | ✅ yes |

So a full port removes essentially **all** of the current dev deps; the only
remaining native dependency in the project would be the **Android NDK** (and only
for Android builds of `platform`/`media`), which is unrelated to the VM and
unavoidable on that target.

Current Rust side: `crates/tjs/src/lib.rs` = **4 724 lines** of C-ABI binding
(`Tjs2Engine`, native-class registration, value retain/release, the
`tjs2_*` `extern "C"` surface) plus the 2 519-line `tjs2_abi.cpp` shim.

## 2. Component map (C++ → Rust)

| Component | C++ (approx LOC) | Rust port notes |
|---|---|---|
| Lexer | `tjsLex.cpp` 1 895 | straightforward; hand-written scanner |
| Grammars/parsers | 3 bison grammars + `tjsInterCodeGen.cpp` 4 125 + `.h` 872 | largest single piece; port grammar to `lalrpop`/`chumsky`/hand-rolled recursive descent, or keep a generated parser |
| Bytecode VM | `tjsInterCodeExec.cpp` 3 123 | register VM; internal bytecode, so we are free to redesign (no wire-compat needed) |
| Object model | `tjsObject.cpp` 2 023, `tjsNative`, `tjsArray.cpp` 1 884, `tjsDictionary.cpp` 808, `tjsNamespace`, `tjsObjectExtendable` | the semantic heart: member lookup order, `Class`/`delegate`, `with`, property getters/setters |
| Variant/string | `tjsVariant.{h,cpp}` 2 596, `tjsVariantString.cpp` 1 048, `tjsString`, `tjsOctPack.cpp` 1 045 | numbers (int/real), string interning, octet type |
| Regex | `tjsRegExp.cpp` → oniguruma | **hardest parity item** (see §4) |
| Serialization | `tjsBinarySerializer.cpp`, `tjsByteCodeLoader.cpp` | only needed if we must load precompiled `.tjs`/OctPack; source games don't |
| Date | `tjsDate.cpp`, `tjsDateParser.cpp` | locale/format source of truth |
| Math/RNG | `tjsMath.cpp`, `tjsMT19937ar-cok.cpp` | MT19937 must match exactly (reproducibility) |
| Script block/cache | `tjsScriptBlock.cpp` 921, `tjsScriptCache.cpp` | |
| Interface/errors | `tjsInterface.cpp`, `tjsNative.cpp`, `tjsError.cpp`, `tjsException.cpp` | |
| ABI shim | `tjs2_abi.cpp` 2 519 | **deleted**; natives become direct Rust calls |

## 3. Why port (benefits)
- **One toolchain.** `cargo build` only; no zig/bison/python/boost/oniguruma.
- **Cross-compilation.** Android no longer needs NDK clang + bison + a C++
  stdlib *for the VM* (only `platform`/`media`'s NDK C APIs remain). Today the
  VM build is the fragile part of the Android story.
- **Safety & debuggability.** No `extern "C"` boundary, no C++ UB (the vendored
  tree already needed `-O2`-unsafe UB fixes), real stack traces, `cargo test`.
- **Smaller/faster.** No libc++ link; native calls become direct.
- **Integration.** Natives register as Rust trait objects, not C function
  pointers; the one process-global ABI binding (D15/`binding.rs`) disappears.

## 4. Hard parts / risks
1. **Regex parity.** TJS uses **Oniguruma** semantics (backreferences,
   lookaround, `\p{}`, Oniguruma-specific syntax). Rust options:
   `regex` (no backrefs/lookaround) ✗; `fancy-regex` (backtracking, backrefs,
   lookaround — close, not Oniguruma-exact); `regress` (ECMAScript) ✗;
   `pcre2` (still C). **Recommendation:** start with `fancy-regex` and keep an
   `onig` feature as the escape hatch; add parity tests for the regex subset the
   test title actually exercises.
2. **String semantics.** TJS strings are UTF-16-ish (`tjs_char`) with specific
   comparison/case/member-name rules; Rust `String` is UTF-8. A port must define
   indexing and `charAt`/`length` semantics to match TJS, not Rust. Big parity
   surface; needs a dedicated conformance suite.
3. **Member lookup / class semantics.** Order of `Class` instance → `delegate`
   → `extends`, `with` scoping, getter/setter invocation, exception class
   hierarchy. Subtle, and scripts depend on it.
4. **Bytecode / OctPack.** Internal, so a Rust VM can use its own IR — *unless* a
   title ships precompiled bytecode/OctPack. The test title ships source; verify
   the target corpus before dropping `tjsByteCodeLoader`/`tjsOctPack`.
5. **Number formatting / date.** TJS `%`-formatting and the date parser are
   byte-exact outputs scripts may compare.
6. **Effort.** ~35–40 k lines of Rust to reach parity, plus a conformance
   corpus. This is a multi-week/month project, not a refactor.

## 5. Rust crate inventory (stand on existing crates)

Do **not** reimplement the mechanical parts. All crates below are current and
heavily used (crates.io, checked 2026-10):

| Component | Crate | Why |
|---|---|---|
| Lexer | **`logos` 0.16** (85 M dl) | derive-based DFA, very fast; keywords/operators/numbers/strings. The regex-literal vs division ambiguity needs lexer modes/callbacks — if that gets awkward, a hand-written scanner tuned like `tjsLex.cpp` is the safe fallback. |
| Parser | **`lalrpop` 0.23** (72 M dl) *or* **`chumsky` 0.13** (29 M dl) *or* hand-written Pratt | `lalrpop` mirrors the existing bison LALR grammar most directly; `chumsky` gives better errors; a hand-written **Pratt** expression parser + recursive-descent statements is fastest and keeps TJS's regex/semicolon quirks exact (recommended). |
| Regex | **`fancy-regex` 0.19** (249 M dl) primary; `onig` (C) as a feature fallback | Backreferences + lookaround (Oniguruma's common features). Lacks possessive/atomic quantifiers, so keep `onig` behind a feature for the rare script that needs full Oniguruma. `regress` is ECMAScript-flavoured — not an Oniguruma substitute. |
| String interning | **`lasso` 0.7** (13 M dl) or **`string-interner` 0.20** (38 M dl) | TJS interns strings (`tjsGlobalStringMap`); interning makes member lookup/compare O(1) on handles. |
| Object/value/frame handles | **`slotmap` 1.1** (112 M dl) | Generational keys → safe handles with cheap insert/remove, no `unsafe` pointer juggling (replaces much of the C++ bookkeeping). |
| Member/property tables | **`indexmap` 2.14** (1.6 B dl) + **`rustc-hash` 2.1** (862 M dl) | TJS preserves member order and looks members up constantly: `IndexMap<K,V,FxBuildHasher>` = insertion order + fast non-crypto hash. V8-style hidden classes are a later step. |
| AST / bytecode / constants | **`bumpalo` 3.20** (624 M dl) | Arena-allocate the AST and constant pool; freed at once, no per-node `Box`. |
| Number ↔ string | **`ryu` 1.0** (1.2 B dl) + **`lexical-core` 1.0** (159 M dl) | Shortest round-trip float formatting (what scripts compare) + fast parse. |
| MT19937 RNG | **`rand_mt` 6.1** | Must match TJS's MT19937 exactly for reproducible `Math.random`/seeding. |
| Object GC / cycles | **`slotmap` arena + explicit mark/sweep**, or **`gc-arena` 0.7** / **`dumpster` 2.1** | TJS is refcounted and cycles need a collector. An explicit mark/sweep over a `slotmap` arena fits the D9 philosophy best. |
| Interpreter | hand-written dispatch over a `Vec<Instr>` (frames in a `slotmap`) | Fastest and simplest; avoid `dyn` in the hot loop. |
| Unicode | **`unicode-segmentation` 1.13** (591 M dl), **`simdutf8` 0.1** (239 M dl) | TJS string semantics are UTF-16 code-unit based — settle the internal representation first (§4.2). |
| Keyword/member fast paths | **`aho-corasick` 1.1** (1.2 B dl) | Only if a scan-based fast path is needed. |

**Perf levers** (crates + design): member lookup on interned handles with
`FxHashMap`/hidden classes; `slotmap` handles instead of `Rc<RefCell>` in hot
paths; arena AST/bytecode; a `match`-on-opcode loop (compiles to a jump table);
and small inline values (int/real inline, string as an interned handle), exactly
like the C++ `tTJSVariant`.

## 6. Strategy (if we proceed)
1. **Differential harness first.** A test rig that runs the same script on the
   C++ VM and the Rust VM and compares value dumps/errors. Keep the C++ VM
   behind a `tjs-cpp` feature until parity; both implement the same `TjsEngine`
   trait. This de-risks every phase.
2. **Port bottom-up:** `Variant`/string/object model + native registration →
   lexer → parser/AST → compiler/IR → VM → stdlib (regex/date/math/serialize).
3. **Parity gates per phase** against the real test title plus a script corpus
   (the existing `crates/tjs/tests` + new cases).
4. **Cut over** when parity holds: delete `cpp/`, `build.rs`, `deps/`,
   `scripts/vendor-tjs2.sh`, and the `tjs-cpp` feature; drop the dev deps.

## 7. Recommendation
Feasible and high-value for build hygiene + Android, but it is the **single
largest remaining work item** in the project — larger than all of L1/L2
combined — with regex and string/class semantics as the real risk. Suggest:
- **Do the differential harness + a thin Rust `TjsEngine` trait now** (small,
  unlocks the port incrementally and immediately improves testability).
- Treat the full port as its own milestone, ported component-by-component,
  rather than a big-bang rewrite.
- Until then, keep the C++ VM; it works and is tested end-to-end on the real
  title.
