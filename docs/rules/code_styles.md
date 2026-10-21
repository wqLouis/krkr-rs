# Code styles — the Rust reviewer checklist

Status: **authoritative** for the Rust level. This file states what a reviewer
accepts and what a reviewer rejects in this repository. Where it disagrees with
[`AGENTS.md`](../../AGENTS.md), `AGENTS.md` wins; where it disagrees with the
design record in [`docs.old/migration/`](../../docs.old/migration), the design record wins and
this file is corrected.

It replaces the older Rust baseline in
[`docs.old/code-conduct.md`](../../docs.old/code-conduct.md): the two must not disagree, and
this file is the one to follow when they do.

Related documents:

| topic | document |
|---|---|
| layer, state, frame and surface contract | [`arch_rules.md`](arch_rules.md) |
| public surface, protocol, ownership | [`api_rules.md`](api_rules.md) |
| the architecture behind the rules | [`docs/arch/`](../arch/) |
| per-crate code maps | [`docs/code/`](../code/) |
| the decisions these rules implement | [`docs.old/migration/README.md`](../../docs.old/migration/README.md) §3 |
| the agent contract and the gate | [`AGENTS.md`](../../AGENTS.md) |

---

## 1. How to use this file

A reviewer walks a module top to bottom and applies the sections in order. The
sections are ordered by how expensive a mistake is:

1. **Errors and fallbacks** (§4) — a silent fallback is a compatibility bug
   that ships as a wrong value instead of a raise.
2. **Layer boundary** — see [`arch_rules.md`](arch_rules.md); a change that
   crosses it is rejected whatever its Rust quality.
3. **Types, functions, naming** (§2–§3) — the shape of the code.
4. **Comments, docs, `unsafe`, tests** (§5–§8) — the surface a reader sees.

A change that mixes a behavioural fix with a style rewrite is not reviewable:
the behavioural fix lands first, alone.

---

## 2. Naming

Names describe **what the code does now**. They do not describe how it relates
to the external engine, how it got that way, or which layer it lives in.

### 2.1 Verbs and nouns

* An action takes a verb; a value takes a noun.
* A function name starts with a lowercase letter. An acronym is not shouted:
  `vm`, `tjs`, `xp3`, `psd` — never `VM`, `TJS`, `XP3`.
* A name is at most three or four words. A name that needs a sentence names a
  function that does too much; split it.
* Do not encode a parameter or return type in the name
  (`parse_string_to_u32_result` is noise).

| concept | use | avoid |
|---|---|---|
| construct a value | `new`, `with_*`, `from_*` | `create_*` for a plain value |
| allocate a native payload | `create` | `make_*` |
| find or resolve | `find_*`, `resolve_*`, `lookup_*` | `get_*` |
| read a property | the property name (`width()`) | `get_width()` |
| change a property under an invariant | `set_*` | a setter that only moves a field |
| advance work | `advance`, `pump`, `step`, `tick` | `do_*`, `process_*` |
| render | `render`, `draw`, `compose`, `present` | `do_render`, `paint_*` |
| storage access | `load`, `read`, `write`, `save`, `remove` | `obtain`, `acquire` |
| ownership hand-off | `take_*`, `release_*`, `retain_*` | raw `add_ref`/`release` |

### 2.2 Vocabulary bans

* No `helper`, `util`, `manager`, `handler`, `processor`, `impl` or `base`
  suffix unless the thing really is that role. Prefer the concrete noun:
  `FontRegistry`, `Scene`, `Transition` — not `FontManager`.
* No `do_`, `handle_` or `process_` prefix when `apply`, `update`, `advance`,
  `dispatch` or a domain verb says more.
* No `tvp`, `TVP`, `L1`–`L4`, decision numbers (`D9`, `D31`, …) or reference
  file names in a production identifier.
* **Exception:** a script-visible member name must match the external engine
  exactly, including its spelling and case. Those names are the contract.

### 2.3 Types, collections, units

* A boolean is a predicate: `visible`, `is_valid`, `has_image`, `can_close`.
  Not `valid_flag`, not `check_*` for something that only reads.
* Collections are plural (`layers`, `entries`); a map says what it maps
  (`by_name`, `by_id`); a count says `count`; an index says `index`, not `idx`.
* A unit belongs in the name or in the type: `timeout_ms`, `width_px`, `gain`
  (not `volume`, when the value is normalized).
* No stutter: `scene.layers`, not `scene_layers`.

---

## 3. Types, functions and signatures

### 3.1 No OOP bean accessors

* A setter whose body is only `self.field = arg` **is** a public field. Delete
  the setter.
* A getter for a public field is noise. Delete it.
* An accessor exists only to enforce an invariant or to do real work, and it is
  named for that work: `clamp_opacity`, `set_interval`. A getter that reads a
  real property is `foo()`, never `get_foo()`.

The `house-style` gate reports a `get_*` whose whole body reads one field, a
`set_*` whose whole body assigns one field, and an `is_*` whose whole body
reads one field.

### 3.2 Ownership and borrowing

Take the weakest useful borrow:

| want | take |
|---|---|
| a string | `&str` |
| a sequence to index or to measure | `&[T]`, `&mut [T]` |
| a path | `&Path` |
| a boxed value | `&T` |
| a sequence only iterated | `impl IntoIterator<Item = T>` (owning) or `impl IntoIterator<Item = &T>` (borrowing) |

`&String`, `&Vec<T>`, `&Box<T>` and `&PathBuf` are rejected. Keep `&[T]` /
`Vec<T>` when the slice or the buffer *is* the contract — indexing, an exact
length, FFI, or a hot path that must not monomorphise.

Accept owned or borrowed input generically where it removes caller noise:
`name: impl Into<String>`, `path: impl AsRef<Path>`,
`items: impl IntoIterator<Item = T>`, `f: impl FnMut(..) -> R`.

### 3.3 Return types

* Return owned data only when ownership is needed.
* A lazy traversal is `impl Iterator<Item = Foo>`; a `Vec` return is for a
  caller that needs indexing, a length or repeated passes.
* A borrowed view is `&[T]` / `&str`.
* Construction that can fail is `Result<impl Iterator<Item = Foo>, Error>`.
* `Box<dyn Iterator>` is not the way to hide a simple `impl Iterator`; use a
  boxed trait object only when the concrete type must be erased.
* `#[must_use]` on pure queries and builders whose result is easy to drop.
* Out-parameters are avoided: return the value, or a struct when several
  values belong together. Native FFI callbacks are the exception — their shape
  is fixed by the ABI.

### 3.4 Shape

* Compose with structs and enums. Do not simulate inheritance with a base
  struct, a `Box<dyn Trait>` hierarchy or a vtable per script class.
* A trait names shared behaviour used through a boundary. A single implementor
  does not need one — an inherent method or a free function is clearer.
* A newtype earns its place when it prevents a unit or identity mix-up
  (`LayerId`, `FontId`, `Milliseconds`). A newtype that only renames a
  `String` does not.
* Use an enum for a closed set of states and match it exhaustively. Do not
  encode a state as a pair of booleans that can disagree.
* Put invariants in constructors and mutators. A public field is preferred
  when there is no invariant.
* Derive `Copy`, `Clone`, `Default`, `Eq`, `Hash` when the type supports it.
  Implement `Default` manually only when the zero value is not meaningful.
  `Eq` requires no floats.
* A generic is justified when it removes a real allocation, a conversion or
  duplication at the call site — not to show off.

### 3.5 Macros and traits

* A macro is justified when the same **structural** boilerplate repeats at
  least three times and a function or trait cannot express it. It must expand
  to readable Rust and carry a doc comment describing the expansion.
* A macro must not hide control flow, error handling or a missing abstraction.
  A macro that generates a whole module is suspect.
* Shared behaviour across several native classes is a **composed struct** plus
  thin forwarding functions — never a registration list copied per class.

---

## 4. Errors and fallbacks

**The single most important review criterion: does this change introduce a
silent fallback?** A silent fallback hides a failure behind a plausible value;
a game then keeps running on wrong data. A typed error is always the better
answer.

### 4.1 The rejected shapes

| shape | why it is rejected | instead |
|---|---|---|
| a wildcard `_ =>` arm that swallows a newly added enum variant | a new failure mode becomes a silent success | an exhaustive `match`, so a new variant is a compile error |
| `unwrap_or(default)` / `unwrap_or_default()` / `unwrap_or_else` on a `Result` that can carry a real error | the failure becomes a default value | propagate the error, or match it explicitly |
| `if let Ok(x) = …`, `.ok()`, `.ok()?` | the error variant is dropped | `match`, or `?` |
| `let _ = fallible_call();` | the failure is invisible | bind and handle it, or make the call infallible |
| `Option`/`bool` where "not found" and "failed" must be told apart | the caller cannot choose | `Result<Option<T>, E>` |
| a numeric cap with no reference basis (`> 256`, `MAX_… = 1024`) | valid large assets break | `checked_*` / `try_from` and a structural validation error |
| `debug_assert!` guarding a data-integrity condition | the check vanishes in release | a real typed error |
| `as usize` / `as u32` on untrusted or script data | the value truncates silently | `try_from` or saturating arithmetic |
| a `todo!` / `unimplemented!` stub in the compatibility layer | the script sees a wrong value | the reference behaviour, or the reference's error |
| `#[allow(dead_code)]`, `#[allow(unused)]` | dead code is kept alive | delete it |

The `error-handling` gate reports these mechanically. The rules are deliberately
narrow so they have few false positives; a finding is a finding.

### 4.2 Wildcard error arms

`Err(_)` / `Err(..)` in a `match` arm, and `if let Err(_) = …`, are the
"discards the error variant" shape. The arm is acceptable only when it
**raises, forwards, transforms, panics or logs** the failure — `report_error`,
`map_err`, `Err(..)`, `?`, a `panic`/`assert`, or a `log::`/`tracing::` call —
or when a `reference/cpp/…` citation documents the mapping next to it.
Anything else is a reviewed allowlist entry, and the allowlist is a to-do list,
not a licence.

### 4.3 Typed errors

* One `#[non_exhaustive]` error enum per domain, owned by the crate that
  declares it: `FsError`, `MediaError`, `RenderError`, `GlyphError`,
  `AudioError`, `TextError`, `SoundError`, `TimerError`, `VideoError`,
  `TransError`, `StorageReadError`, `MountError`, `StartupError`, `ConfigError`.
* Errors are **values**. No panic on bad input, no `unwrap` on a value a
  script or a file can influence.
* A new failure mode is a new variant in the owning enum, handled at the call
  site. It is never a new default value.
* The compatibility layer has no stubs: a member either behaves like the
  reference or raises exactly where the reference raises. The Android backend
  is the one exception, because no script can observe it.

### 4.4 A pointer is never a handle

* A value that may carry a pointer or an address-derived value is never
  sign-checked (`> 0`, `>= 0`, `.max(0)`, `abs()`), never narrowed
  (`as u32` / `as i32`), never round-tripped through `NonZero*` / `try_from`.
* Where the external engine hands a script an address as an integer
  (`Window.HWND`, `Bitmap.buffer`, `Rect.nativeArray`, `DrawDevice.interface`,
  the layer image-buffer members), the port mirrors it and treats the number as
  an opaque token. No code reads it back through a sign test.
* A handle that crosses script-visible state, or that state is keyed by, is a
  stable opaque **id**, never an address: addresses are reused, differ in sign
  by platform, and on aarch64 Android the heap is above the sign bit while
  x86-64 Linux puts it below.
* A native plugin member reaches another plugin object through the ABI's
  native-instance mechanism, never through a hidden script property that
  stores a pointer as an integer.

The `error-handling` gate's `pointer-sign`, `pointer-flow`, `pointer-compare`,
`char-signedness` and `addr-narrow` rules enforce this.

---

## 5. `unsafe`

* `unsafe` is **forbidden** in the pure crates: `api`, `fs`, `engine`, `testkit`
  and the `psd` container crate (`#![forbid(unsafe_code)]`).
* It is **denied** at the crate root elsewhere (`#![deny(unsafe_code)]`), with
  a file-level `#![allow(unsafe_code)]` only where the FFI or Vulkan truly
  lives: the native ABI seam under `crates/compat/src/`, the ash/Vulkan
  boundary under `crates/platform/render/src/vulkan/`, the TJS2 C ABI host
  (`crates/vm/src/host.rs`) and the video-decoder seam
  (`crates/app/src/video.rs`). A file-level allow anywhere else is a finding.
* Every `unsafe` block carries a `// SAFETY:` note that explains the invariant
  at that call site — what is valid, for how long, and why the aliasing or
  lifetime rules hold. An `unsafe fn` is documented either by a `# Safety`
  doc section or by such a note.
* A raw pointer stays inside the FFI adapter and is converted to a safe handle
  immediately. Raw-pointer structs do not travel in place of a lifetime.
* Manual `AddRef`/`Release` pairs are rejected in Rust-owned paths; ownership,
  `Rc`/`Arc` or an explicit registry is used where the FFI contract demands
  it, with the ownership documented in the `// SAFETY:` note.

The `layering` gate reports a missing safety note and a file-level allow
outside the reviewed locations.

---

## 6. Comments, docs and log text

### 6.1 A comment describes the code as it is

* Say how the code works now and what the non-obvious invariant is.
* Delete history ("was", "used to", "previously", "before this", "replaced by"),
  design rationale, decision numbers (`D9`, `D31`, …) and cross-links to the
  migration documents. History is read with `git`.
* A comment that repeats the code is deleted.
* A comment that explains a non-obvious *why* is kept, in the present tense.

The `house-style` gate reports a decision number, a history phrase, and the
external-engine terms (`reference/cpp`, `reference`, `oracle`, `C++`, `TVP`,
`KiriKiri`, `ported`, `port of`, `upstream`, `original implementation`) in a
**production** comment.

### 6.2 Reference citations live in tests

A behaviour assertion in a test carries its citation, formatted
`reference/cpp/...:line`. The implementation owes no citation and no
resemblance to the external source: the test is the oracle, the code is free.

A production comment must not cite the external engine at all. Legacy
comments that predate this rule are recorded in
`scripts/allowlists/house_style_baseline.txt` and burn down over time; a new
one fails the gate.

### 6.3 Log and print text

* Plain English. No `L1`–`L4`, no decision numbers, no external class names
  unless the name is script-visible.
* The `house-style` gate reports a decision number or a layer number inside a
  `log::`/`println!`/`eprintln!` literal.

### 6.4 Public documentation

* Every public item has a doc comment. Every crate root carries
  `#![warn(missing_docs)]`; the `layering` gate checks the crate roots.
* A doc comment states the contract, the units and the failure mode:
  `# Errors` for a fallible function, the coordinate space for a pixel value,
  the lifetime requirement for an attached handle.
* A module doc explains the module's job, not its past.
* An item that is not part of the intended contract is not re-exported. A flat
  glob re-export (`pub use crate::…::*;`) is rejected by the `house-style`
  gate; re-export deliberately and name the items.

---

## 7. Dependencies, macros and traits

* No new workspace dependency without a reason stated in the change
  description. `std` and the existing workspace crates are enough unless a
  measured requirement needs more.
* Performance work is welcome, provided a reference-cited test still holds and
  the behaviour the script observes does not change.
* A trait exists to name behaviour several types share and callers use
  generically, or to mark an L1/L2 boundary. A trait with one implementor is
  a function.
* A macro exists to remove repeated *structural* boilerplate (three or more
  occurrences). It must not hide control flow or error handling.

---

## 8. Tests

* Tests live in `tests/` (integration) or in a `#[cfg(test)]` module.
* **Every behaviour assertion cites the reference**: `reference/cpp/...:line`
  above the assertion. A test without a citation is not evidence; do not add
  one.
* A regression fix gets a test that would have failed before it.
* Drive the real public surface — the VM, `Env::eval` — rather than a
  hand-built intermediate, for behaviour that belongs to the compatibility
  layer.
* The shared harness helpers live in `crates/compat/tests/common/mod.rs`:
  `eval_then` for a write-then-read (an evaluation compiles in expression mode,
  so a write and a read are two executions), `non_primary_layer`, and the VM
  lock. Use them.
* The compatibility binding is process-global and single: drop the first
  environment before creating a second in one test.
* Script-level value spellings are the external engine's: `typeof` answers
  `"Object"`, `"Integer"`, `"Real"`, `"String"`, `"Octet"`, `"void"`; `void`
  is not `undefined`; an absent member is `"undefined"` while a present but
  unset member is `"void"`.
* A native property getter returns what the reference returns. A backing
  `StoredValue` field whose constructor default is `StoredValue::Void` must be
  mapped to the reference's non-void value (an empty array, a dictionary, the
  class's own object) before it reaches the script — otherwise reading a
  member from it raises. A property whose reference getter genuinely reads
  back void is recorded in `scripts/allowlists/void_getter_allow.txt`.

---

## 9. The review procedure

For each module, in order:

1. Read the module and its callers. Name one job per function and one owner
   per piece of state.
2. List the anti-patterns above: long or compound names, trivial accessors,
   borrowed collections, manual loops that should be iterators, duplicated
   marshalling, C++-shaped types, history comments, a silent fallback.
3. Rewrite the smallest set of functions that removes the pattern **without
   changing behaviour**. A script-visible value, an error, a member name and
   the registration surface stay exactly as they are.
4. Run `cargo fmt`. Run the compiler-free checks of
   [`AGENTS.md`](../../AGENTS.md) §0.4 — `stateless`, `error-handling`,
   `house-style`, `layering`, `storage-boundary`, `native-construction`,
   `lifecycle-order`, `storage-media`, `callback-objthis`, `native-arity`,
   `void-getter`, `host-facts`.
5. Report: the files changed and the exact scope; the patterns fixed with the
   behaviour-preserving argument; the behaviour risk and what was deliberately
   left alone; the blockers, or a bug that belongs to another scope.

The main agent runs the build, clippy and test gate. A change is not done
until both the static gate and the test suites are green.

---

## 10. What the gate enforces

| check | the rule it protects |
|---|---|
| `cargo fmt --all -- --check` | formatting |
| `cargo clippy --workspace --all-targets -- -D warnings` | every lint, including the pedantic ones; no `#[allow(clippy::…)]` is possible (the `house-style` gate rejects one in any Rust file, tests included) |
| `stateless` | §4 of [`arch_rules.md`](arch_rules.md): no process-global state in L1, no `&mut self` on an L1 service |
| `error-handling` | §4: no parse-then-default, no discarded error, no `debug_assert!` on data integrity, no pointer sign test or narrowing, no wildcard error arm that swallows |
| `house-style` | §2, §3.1, §6: bean accessors, `&String`/`&Vec`/`&Box` parameters, glob re-exports, decision numbers, external-engine citations and history in production comments, jargon in log text, clippy allowances |
| `layering` | §5, §6 and [`arch_rules.md`](arch_rules.md): layer vocabulary, platform code in L3, the `engine_api` seam, `SAFETY:` notes, the file-level `unsafe` allow, `missing_docs` |
| `storage-boundary` | a script-supplied storage name is stripped of its media prefix before it crosses into L2 or the host filesystem |
| `storage-media` | every `scheme://` domain the reference registers is served or explicitly documented as missing |
| `native-construction` | a native payload carries the state its members need the moment it exists |
| `lifecycle-order` | a native owner finalizes its child script objects before it removes their records |
| `callback-objthis` | a member that invokes a caller's callback receives the caller's `objthis` |
| `native-arity` | a native method whose reference form takes zero arguments has a zero-argument branch |
| `void-getter` | §8: a native getter never hands the script a raw `void` where the reference returns a value |
| `host-facts` | every host-fact seam on the compatibility host is installed by production code |
| `surface-lock` | the script-visible surface is the committed baseline |
| `native-parity`, `plugin-parity` | reports: what the reference has that the port does not |

---

## 11. Definition of done

1. Behaviour matches the reference (or raises exactly where it does), and every
   behaviour test cites it.
2. No silent fallback, no arbitrary cap, no `#[allow(dead_code)]`, no stub.
3. The layer and dependency rules hold.
4. The script-visible surface and both parity reports are unchanged, or the
   baseline change is deliberate and reviewed.
5. Comments describe the code as it is; public items are documented;
   `cargo fmt` is clean.
6. The superseded code, test, allowlist entry and document are **deleted in the
   same change**. No shim, no stubbed old signature, no "kept for reference"
   copy, no appended "formerly" section.
7. The main agent's full gate is green.
