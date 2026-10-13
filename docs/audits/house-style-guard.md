# House-style guard (AGENTS §5 / §6)

- Repo: `/mnt/DATA/Document/code/krkr-rs`
- Branch: `migration/middle-layer-ash` (working tree)
- Date: 2026-10-13
- Guard: `scripts/check-house-style.sh`
- Allowlist: `scripts/house_style_allow.txt`
- Fixtures: `scripts/house_style_fixtures/{bad_style,good_style}.rs`
- Scope: every non-test `.rs` under `crates/**` (excluding `crates/tjs/cpp/**`).
- **No `cargo`/`rustc`/`clippy` command was run** (AGENTS §0). The scan is the
  Python program embedded in the bash guard, modelled on
  `scripts/check-error-handling.sh`.

House rules referenced: AGENTS §5 (idiomatic Rust / house style), §6 (comments,
docs and naming), D31 (comments describe the code as it is), D30 (reference
citations live in tests), D11 (no silent fallback).

---

## 1. Why a new guard

The only automated idiomatic checks were `cargo fmt`, `cargo clippy -D
warnings`, the D9 stateless guard and the AGENTS §4 error guard. The §5/§6
rules — no bean accessors, no `&String`/`&Vec<T>`/`&Box<T>` parameters, no
decision numbers in comments, no whole-crate glob re-export, no internal jargon
in logged text — rested on review alone. This wave showed review alone is not
enough, so `scripts/check-house-style.sh` mirrors the error guard's shape (a
compiler-free Python scan inside a bash script, an allowlist with rule-name and
code-anchor entries, stale-entry reporting, `--print-all`, and a `--self-test`
that runs before every scan).

## 2. Rules

Every rule must be decidable from syntax with acceptable precision; a rule that
flagged hundreds of sites would be worse than no rule.

| rule | catches | cannot catch |
|---|---|---|
| `bean-accessor` | a `get_*`/`set_*`/`is_*` method whose body is exactly `self.field = arg;`, `self.field`, `&self.field`, `&mut self.field`, or `Ok(self.field)` | whether the accessor "does real work" in a way the tokenizer does not see (the body is exact, so a body with one added operation is not flagged); a method not prefixed `get_`/`set_`/`is_` that shuttles a field; whether a private-field getter is the intended read API or a redundant wrapper |
| `ref-collection-param` | an immutable `&String`, `&Vec<T>`, `&Box<T>` or `&PathBuf` in a parameter list | `&mut Vec<T>` / `&mut String` (a growable buffer cannot always become `&mut [T]` / `&mut str`); a fully-qualified `&std::string::String`; a `&String` in a non-parameter position (a local, a return type, a `Vec<&String>` element) |
| `decision-number` | a 1–3 digit decision number (`D9`, `D31`, `D16`) inside a code or doc comment | decision numbers in test files and `#[cfg(test)]` items (exempt by the rule); a decision number in a string literal (that is `jargon-in-log`'s job); a reference to a decision by name with no number |
| `glob-reexport` | `pub use crate::…::*;` and `pub use <extern-crate>::…::*;` | whether the re-export is deliberate; `self::`/`super::` module globs and non-`pub` globs (not whole-crate contracts, so not flagged) |
| `jargon-in-log` | `L1`–`L4` or a decision number inside a string literal passed to `log::trace!`/`debug!`/`info!`/`warn!`/`error!`, `println!`, `print!`, `eprintln!`, `eprint!` | jargon in a message assembled by `format!` and logged later; jargon in a non-logging string (a typed error's text, a member name) |

The comment/string handling follows the error guard: a single `scan_spans()`
classifier finds comments and literal spans, `mask()` blanks their contents
while preserving length and newlines so offsets and line numbers stay valid,
and `test_ranges()` finds `#[cfg(test)]` items to exempt. `decision-number` and
`jargon-in-log` read the *unmasked* spans; the other rules read the masked copy.

## 3. Self-test

`scripts/check-house-style.sh --self-test` scans
`scripts/house_style_fixtures/` and asserts:

* `bad_style.rs` trips **all five** rules (a trivial getter, an `Ok` getter, a
  trivial setter, an `is_` getter, all four banned reference parameters, a
  decision number in a comment, a `pub use crate::…::*;`, and `"L2"` in a
  `log::warn!`), and
* `good_style.rs` trips **none** (a `get_` that does real work, a clamping
  setter, a combined `is_` predicate, `&[u8]` / `&str` / `&dyn Fn()` / `&Path`
  parameters, a non-`pub` glob and a `self::` glob, the letter `D` + digits in a
  string and in a hex-looking comment, and a log string with no jargon).

The self-test runs before every scan, so a regex that silently stops matching
fails the gate instead of passing silently. If it fails the guard prints which
rule stopped matching and exits non-zero.

```console
$ time bash scripts/check-house-style.sh --self-test
house-style guard: self-test ok (5 rules flagged the bad fixture, 0 finding(s) in the good fixture)
elapsed 0.032 s
```

The full tree scan (self-test included) is ~2.0 s on this host:

```console
$ time bash scripts/check-house-style.sh --print-all >/dev/null
elapsed 2.007 s
```

## 4. Triage

## 4. Triage

The triage ran against the moving `crates/**` tree on 2026-10-13. The
**first** `--print-all` pass found nine sites:

| rule | sites | real violation | acceptable | false positive |
|---|---:|---:|---:|---:|
| `bean-accessor` | 6 | 0 | 6 | 0 |
| `ref-collection-param` | 0 | 0 | 0 | 0 |
| `decision-number` | 3 | 3 | 0 | 0 |
| `glob-reexport` | 0 | 0 | 0 | 0 |
| `jargon-in-log` | 0 | 0 | 0 | 0 |
| **total** | **9** | **3** | **6** | **0** |

The six `bean-accessor` findings are all `is_*` boolean queries over a
**private** field (`is_headless`, `is_bound`, `is_paused`, `is_done`,
`is_complete`, `is_playing`). AGENTS §5 deletes "a getter for a **public**
field"; these fields cannot be public because their mutators keep a multi-field
invariant (a public `bound` would let a caller defeat the unsafe-dereference
guard; a public `paused`/`done`/`completed`/`playing` would allow an
inconsistent state). The accessor *is* the read API, so it is not a bean
accessor; each is allowlisted with its reason in
`scripts/house_style_allow.txt`. There are no `get_*`/`set_*` pure field-movers
in the tree.

The three `decision-number` findings were real D31 violations in `crates/**`:
`crates/android/src/config.rs:15` (`… default (D11).`),
`crates/compat/src/natives/system.rs:1453` (`… executable path (D18).`) and
`crates/engine/src/storage.rs:1652` (`… to the reference (D15 / doc 09): the`).
A fourth site, `crates/compat/src/plugin.rs` (`D14` in the `autoload` doc
comment and its warning string), had already been removed by the concurrent
house-style wave before the first pass finished. The same wave removed the
remaining three decision numbers before the guard was wired in, so the
**final** state is:

| rule | sites | real violation | acceptable | false positive |
|---|---:|---:|---:|---:|
| `bean-accessor` | 6 | 0 | 6 | 0 |
| all other rules | 0 | 0 | 0 | 0 |
| **total** | **6** | **0** | **6** | **0** |

```console
$ bash scripts/check-house-style.sh
house-style guard: clean (6 site(s) checked, 6 allowlisted) — scripts/house_style_allow.txt
```

Before this change the only automated coverage of these five rules was
`cargo fmt`/`clippy` (none of them) and review. After it, all five are
mechanically enforced, and the only manual residue is the six reviewed
exceptions.

## 5. Wiring

The guard is wired into the gate immediately after the §4 guard:

```bash
bash scripts/check-stateless.sh                  # D9 guard
bash scripts/check-error-handling.sh             # AGENTS §4 guard
bash scripts/check-house-style.sh                # AGENTS §5/§6 guard
```

`AGENTS.md` §0 and `TODO.md` §1 both carry the new step. The guard exits zero
on the current tree (six reviewed `bean-accessor` exceptions, no other sites).

## 6. Honest limits

* **The scan is syntax-level.** It cannot decide whether an accessor does real
  work, whether a re-export is a deliberate contract, or whether a
  private-field getter is intended. It over-reports on those shapes and relies
  on the allowlist; a missed regression is treated as worse than a reviewed
  exception.
* **`bean-accessor` is exact-body only.** A trivial accessor with one extra
  operation, or a `get_*`/`set_*` method that is not a field-mover, is not
  flagged. It also only considers the `get_`/`set_`/`is_` prefixes; a
  differently-named field-mover passes.
* **`ref-collection-param` sees tokens, not resolved types.** A
  fully-qualified `&std::string::String`, a type alias for `String`, or a
  macro-generated parameter list is missed. `&mut Vec<T>` is deliberately not
  flagged.
* **`decision-number` matches `D1`–`D999`.** A four-digit decision number would
  need the regex updated. It intentionally does not flag a decision number in a
  test file or `#[cfg(test)]` item.
* **`glob-reexport` does not judge intent.** A legitimate whole-crate facade
  must be allowlisted with a reason.
* **`jargon-in-log` only sees direct literal arguments.** Jargon built by
  `format!` and logged indirectly, or in a non-logging string, is out of scope.
* **The triage is a snapshot.** The working tree was being edited concurrently
  and four decision-number sites disappeared mid-triage, so counts and line
  anchors should be re-checked after any `crates/**` wave.
* **No cargo was run**, per AGENTS §0. The guard itself is Python and shell;
  `cargo fmt --check` still has to confirm the guard's own files are fmt-clean
  (they are not workspace members, so they are not part of the fmt run).
