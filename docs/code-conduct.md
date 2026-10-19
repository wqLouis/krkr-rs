# Code conduct — Rust review and rewrite rules

This is the review checklist for code in this repository. `AGENTS.md` remains
the contract; this file makes the Rust-level expectations concrete. If the two
disagree, `AGENTS.md` and `docs/migration/**` win.

The engine is a compatible reimplementation, but the implementation is **Rust**,
not a transliteration of the reference C++. Preserve script-observable behavior;
write the cleanest Rust that expresses it.

---

## 0. Agent baseline — what a worker writes and does

A worker delivers one small, reviewable, behavior-preserving change in an
explicit scope. The main agent owns the design, the integration and the gate;
the worker owns the implementation of its slice.

Before writing code, the worker states:

- the one job of the change and the file(s) it belongs in;
- the reference behaviour or design contract that makes it correct;
- the state owner and the layer that must own it;
- what must **not** move: script-visible values, member names, errors, ABI,
  surface lock, public API;
- the smallest verification that proves it.

When writing code, prefer this shape:

```rust
// A value with an invariant.
struct CacheLimit { /* ... */ }

// One explicit action; the weakest useful borrows.
fn trim_to(&mut self, limit: CacheLimit) -> usize { /* ... */ }

// Composition and exhaustive state.
enum CacheMode { Disabled, Unlimited, Bytes(u64) }
```

Use structs, enums, free functions and explicit state. Do not recreate a class
hierarchy, a `Manager`, a vtable, an `*Impl`, or a C++ ownership pair. Use a
trait or macro only when the same structure genuinely repeats at least three
times; otherwise a helper function is clearer.

The worker **does not**:

- change behaviour, public/script-visible names or the surface baseline to make
  a cleanup easier;
- fix an unrelated bug found on the way — report it instead;
- add a dependency, a global, a hidden cache or a fallback;
- keep the superseded code, test, allowlist entry or doc "for compatibility"
  — deleting it is part of the change, not a follow-up;
- widen its own write scope or edit a file another worker owns;
- run `cargo build`, `cargo check`, `cargo clippy`, `cargo test`, `cargo run`,
  or `rustc`; the main agent runs those.

The worker finishes by running `cargo fmt` and the compiler-free guards listed
in §8, then reports:

1. files changed and the exact scope;
2. patterns fixed, with the behavior-preserving argument;
3. behavior risk and anything deliberately left alone;
4. blockers, or a bug that belongs to another scope.

A worker result is a proposal. The main agent reads the diff, checks the layer
boundary and the reference, and runs the full gate before committing.

---

## 1. Non-negotiables

- Preserve behavior. A refactor must not change a script-visible value, an
  error, a member name, or the registration surface.
- Keep the layer boundaries and dependency direction from `AGENTS.md` §2.
- No silent fallback, no arbitrary cap, no discarded error (`AGENTS.md` §4).
- No process-global state in L1; all engine state is owned by L2.
- No `#[allow(clippy::…)]`, `#![allow(clippy::…)]` or `#[expect(clippy::…)]`,
  in production or tests: fix the lint or restructure the code. The
  `house-style` gate records one as a hard finding and never allowlists it.
- L3 is VM-facing glue: marshal arguments, call one L2 operation, marshal the
  result. Do not grow L3 into a second engine.
- Android-only stubs are the sole stub exception. L3 has no stubs.
- Do not add a dependency for a cleanup. `std` and the workspace crates are
  enough unless a measured requirement needs more.
- Do not change the public surface to make an internal cleanup convenient.

---

## 2. Naming

Names describe **what the code does now**, not how it relates to the C++
reference, not its history, and not its layer number.

Use one vocabulary consistently:

| concept | preferred verb | avoid |
|---|---|---|
| construct a value | `new`, `with_*`, `from_*` | `create_*` for a plain value |
| allocate a native payload | `create` (the callback name may keep the reference name) | `make_*` |
| find/resolve | `find_*`, `resolve_*`, `lookup_*` | `get_*` |
| read a property | the property name (`width()`, `visible()`) | `get_width()` |
| change a property with an invariant | `set_*` if it really enforces something | a setter that only moves a field |
| advance work | `advance`, `pump`, `step`, `tick` | `do_*`, `process_*` |
| render | `render`, `draw`, `compose`, `present` | `do_render`, `paint_*` when `render` is enough |
| storage access | `load`, `read`, `write`, `save`, `remove` | `obtain`, `acquire` |
| ownership hand-off | `take_*`, `release_*`, `retain_*` | raw `add_ref`/`release` spellings |

Rules:

- Use a verb for an action and a noun for a value. A function name starts with
  a lowercase letter; an acronym is not shouted (`vm`, `tjs`, `xp3`).
- Keep names short but unambiguous. Aim for **at most three or four words**;
  if a name needs a sentence, the function probably does too much. Do not
  encode parameter types or the return type in the name
  (`parse_string_to_u32_result` is noise).
- Test names are descriptive sentences that state the behavior under test;
  they are exempt from the word limit. Keep the reference citation next to the
  assertion, not in the test name.
- No `helper`, `util`, `manager`, `handler`, `processor`, `impl`, or `base`
  suffixes unless the thing really is that role. Prefer the concrete noun:
  `FontRegistry`, `Scene`, `Transition`, not `FontManager`.
- No `do_`, `handle_` or `process_` prefixes when `apply`, `update`, `advance`,
  `dispatch` or a domain verb says more.
- No `tvp`, `TVP`, `L1`–`L4`, decision numbers, or reference filenames in
  production identifiers. Script-visible member names are the exception: they
  must match the reference exactly.
- A boolean is a predicate: `visible`, `is_valid`, `has_image`, `can_close`.
  Do not call a predicate `valid_flag` or a command `check_*` when it only reads.
- Collections are plural (`layers`, `entries`); maps say what they map
  (`by_name`, `by_id`); counts say `count`; indices say `index`, not `idx`.
- Units belong in the name or type: `timeout_ms`, `width_px`, `gain` (not
  `volume` when the value is normalized).
- Avoid stutter: in `scene.layers`, use `layers`, not `scene_layers`.

---

## 3. Functions and signatures

- Prefer small functions with one job. A function should be readable top to
  bottom without scrolling through repeated argument marshalling.
- Take the weakest useful borrow:
  - `&str`, not `&String`;
  - `&[T]` / `&mut [T]`, not `&Vec<T>` / `&mut Vec<T>` unless capacity is used;
  - `&Path`, not `&PathBuf`;
  - `&T`, not `&Box<T>`.
- A sequence parameter that is only iterated is `impl IntoIterator<Item = T>`
  (owning) or `impl IntoIterator<Item = &T>` (borrowing), not `Vec<T>` or
  `&[T]`; keep the slice when the slice itself is the contract (indexing,
  exact length, FFI, or a hot path that must not monomorphise).
- Accept owned or borrowed input generically where it removes caller noise:
  - `name: impl Into<String>` for an owned stored string;
  - `path: impl AsRef<Path>` when any path-like value is valid;
  - `items: impl IntoIterator<Item = T>` when the function consumes a sequence;
  - `f: impl FnMut(..) -> R` rather than `&dyn Fn` unless type erasure is
    required.
- Return owned data only when ownership is needed. Prefer:
  - `impl Iterator<Item = Foo>` for a lazy traversal — a `Vec` return only
    when the caller needs ownership, indexing, a length or repeated passes;
  - `&[T]` / `&str` for a borrowed view;
  - `Result<impl Iterator<Item = Foo>, Error>` when construction can fail.
  Collect only at a boundary that needs a slice, a count, or sorted order.
- Do not return `Box<dyn Iterator>` to hide a simple `impl Iterator`; use a
  boxed trait object only when the concrete iterator type must be erased
  (for example a public API with many branches and a stable type).
- Use `#[must_use]` on pure queries and builders whose result is easy to drop.
- Avoid out-parameters. Return the value, or return a struct when several
  values belong together. Native FFI callbacks are the exception: their
  out-pointer shape is fixed by the ABI.
- Prefer `&self` methods for queries and explicit `&mut` state arguments for
  services. Do not add hidden `&mut self` state to an L1 service.
- Do not make a function generic just to show off. Generics must remove a
  real allocation, conversion, or duplication at the call site.

---

## 4. Types and abstraction

- Compose with structs and enums. Do not simulate inheritance with a base
  struct, a `Box<dyn Trait>` hierarchy, or a vtable per script class.
- A trait exists to name shared behavior used through a boundary, not to group
  unrelated methods. Prefer a free function or an inherent method when there is
  one implementor.
- Use a newtype when it prevents a units/identity mix-up (`LayerId`, `FontId`,
  `Milliseconds`). Avoid newtypes that only rename a `String` without adding an
  invariant.
- Use enums for a closed set of states; match them exhaustively. Do not encode
  state as a pair of booleans that can disagree.
- Put invariants in constructors and mutators. A public field is preferred when
  there is no invariant; a getter for a public field is noise.
- Derive `Copy`, `Clone`, `Default`, `Eq`, `Hash` when the type supports it.
  Implement `Default` manually only when the zero value is not meaningful.
- Use `TryFrom`/`checked_*` for narrowing and size math. Use `From`/`Into` for
  widening. Never let an unchecked `as` truncate a script-supplied value.
- Prefer `Option<T>` for absence and `Result<T, E>` for failure. Do not use
  `bool` to mean “found or error”; `Result<Option<T>, E>` distinguishes the two.
- A native property getter returns what the reference returns. A `StoredValue`
  field whose constructor default is `StoredValue::Void` must be mapped to the
  reference's non-void value (an empty array, a dictionary, the class's own
  object) before it reaches the script: a raw passthrough makes
  `obj.prop.member` raise on a fresh object. The `void-getter` gate enforces
  this; a reference property that genuinely reads back void is recorded in
  `scripts/allowlists/void_getter_allow.txt`.
- No `static mut`, `thread_local!`, `LazyLock`, `OnceLock`, or hidden registries
  in L1. Per-engine state belongs in the explicit state object.

---

## 5. Macros and traits: use them to remove real boilerplate

- A macro is justified when the same **structural** boilerplate repeats at
  least three times and a function/trait cannot express it. It must expand to
  readable Rust and carry a doc comment describing the expansion.
- Do not use a macro to hide control flow, error handling, or a missing
  abstraction. A macro that generates an entire module is suspect.
- A trait is justified when several types share a behavior that callers use
  generically, or when it is an L1/L2 boundary. Do not introduce a trait for a
  single implementation.
- Prefer a small generic helper or a closure over a macro when the shape is
  ordinary Rust.
- Shared behavior across L3 native classes is a **composed struct** plus thin
  forwarding functions, never a copied registration list per class.

Examples of the preferred direction:

```rust
// Accept a sequence without forcing a Vec at every call site.
pub fn retain_layers(&mut self, ids: impl IntoIterator<Item = LayerId>) { /* ... */ }

// Borrow a view; do not allocate just to return it.
pub fn visible_layers(&self) -> impl Iterator<Item = &Layer> {
    self.layers.values().filter(|layer| layer.visible)
}
```

---

## 6. No OOP patterns, no C++ transliteration

Reject these shapes:

- `struct FooManager` with `get_bar`/`set_bar` methods that only move fields.
- A “base class” struct embedded in every derived struct with virtual-function
  tables, `*_impl` names, or `this`-pointer callbacks.
- C++-style `doSomething`, `SetSomething`, Hungarian prefixes, `m_`, `p_`,
  `lp`, `b`, `n` prefixes, or all-caps enum constants (`LT_ALPHA`) in L2/L1.
- Manual `AddRef`/`Release` pairs in Rust-owned paths. Use ownership,
  `Rc`/`Arc`, or an explicit registry only where the FFI contract demands it;
  document the ownership in a `// SAFETY:` note at the unsafe boundary.
- Raw-pointer structs passed around as a substitute for a lifetime or a typed
  handle. Keep raw pointers inside the FFI adapter and convert to a safe handle
  immediately.
- A direct line-by-line copy of a C++ function when a Rust iterator, enum,
  `match`, or ownership transfer expresses the same behavior more clearly.
  The reference is the behavioral oracle, not the source shape.
- `impl` blocks that exist only to expose private fields to a sibling module;
  restructure the module boundary instead.
- Comments that narrate the C++ translation or the change history
  (“before”, “previously”, “now uses”, “replaced”, “decision D…”).

---

## 7. Comments and docs

- A comment describes the current code and the non-obvious invariant. Delete
  history, rationale essays, “before/after” comparisons, decision numbers, and
  links to migration docs.
- Keep `// SAFETY:` on every unsafe block, and make it explain the actual
  invariant at that call site.
- Keep reference citations in tests, formatted `reference/cpp/...:line`.
  Production code does not cite the reference.
- The `house-style` gate enforces the paragraph above for non-test comments: a
  comment that names the external engine (`reference/cpp`, a `*.cpp:line` or
  `.h`/`.hpp` filename, `C++`, `TVP`, `KiriKiri`, `ported`, `upstream`,
  `port of`) or narrates history (`used to`, `previously`, `before this`,
  `replaced by`) is a finding. Existing comments that predate the rule are
  baselined in `scripts/allowlists/house_style_baseline.txt` (burn-down in
  `TODO.md` §15); a new one fails.
- Public items have doc comments. Module docs explain the module’s job, not its
  past.
- A comment that repeats the code is deleted. A comment that explains a
  non-obvious “why” is kept, phrased in the present tense.
- Log messages are plain English: no `L1`–`L4`, no decision numbers, no C++
  class names unless the name is script-visible.

---

## 8. Review procedure

For each module, in order:

1. Read the module and its callers. Identify one job per function and one
   owner per state.
2. List anti-patterns from this document: long/compound names, trivial
   accessors, borrowed collections, manual loops that should be iterators,
   duplicated marshalling, C++-shaped types, history comments.
3. Rewrite the smallest set of functions that removes the pattern without
   changing behavior. Prefer a trait/macro only when it removes repeated
   structure.
4. Run `cargo fmt` (allowed for subagents). Run the compiler-free guards:
   `stateless`, `error-handling`, `house-style`, `layering`, `storage-boundary`,
   `native-construction`, `lifecycle-order`, `storage-media`,
   `callback-objthis`, `host-facts`.
5. The main agent runs the build, clippy and test gate. A refactor is not done
   until both static and test gates are green.

Do not mix a behavioral fix with a style rewrite. If a rewrite exposes a bug,
stop and report it; fix behavior in a separate change.

A rewrite lands as a replacement, not an addition: the old implementation,
its tests, its allowlist entries and the doc that described it are removed in
the same change. Git keeps the history; the tree does not.
