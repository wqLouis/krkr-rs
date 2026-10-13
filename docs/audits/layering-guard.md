# Layering and `unsafe` guard

- Repo: `/mnt/DATA/Document/code/krkr-rs`
- Branch: `migration/middle-layer-ash`
- Guard: `scripts/check-layering.sh`
- Allowlist: `scripts/layering_allow.txt`
- Fixtures: `scripts/layering_fixtures/`
- Date: 2026-10-13
- House rules referenced: `AGENTS.md` §2 (the four layers and the one allowed
  dependency direction), §5 (idiomatic Rust / the `unsafe` policy), §0 (the
  gate) and §10 (definition of done).

This is the audit of the guard, not of the code: no source outside the guard,
its fixtures, its allowlist and this document was modified. The scan is
compiler-free (python3 inside a bash wrapper); no `cargo`/`rustc`/`clippy`
was run, per `AGENTS.md` §0.

---

## 1. Rules implemented

Six syntax-level rules. The guard mirrors `scripts/check-error-handling.sh`:
it blanks comments and string/char literals, skips `#[cfg(test)]` items where
the rule says "outside tests", prints `file:line:rule: code`, exits non-zero on
an unallowlisted finding, reports stale allowlist entries, and supports
`--print-all` and `--self-test`.

| rule | scope | catches | cannot catch |
|---|---|---|---|
| `l1-vocabulary` | `crates/platform/{api,fs,media,render,platform}/src/**` | `TVP_`, `LT_`, `TJS`, `kag`/`KAG` identifiers in code; `VK_` outside `render` | a token that merely *resembles* TVP vocabulary, or whether a matched identifier is actually used as TVP knowledge; `Layer`/`VK_`/`Storage` in `render`; anything inside a comment or string |
| `l2-vocabulary` | `crates/engine/**` (tests too) | `TVP_`, `LT_`, `TJS`, `kag`/`KAG`, `VK_`, and the literal `100000` (the TVP volume scale) | a volume scale expressed as `0..=100000` another way; `arc.xp3>path` addressing in a comment (intentional: the L2 storage layer documents the address syntax); anything in a literal |
| `l3-platform` | `crates/compat/src/**` | `#[cfg(target_os`, `winit`, `android-activity`, `std::fs`, `jni`, `ndk` | platform code reached through an L1 abstraction (correctly out of scope); `crates/compat/tests/**` is deliberately out of scope — tests build fixtures with `std::fs` |
| `unsafe-no-safety-note` | all `crates/**` except tests and `build.rs` | an `unsafe { … }` block or `unsafe fn` with no `// SAFETY:` note (or `# Safety` doc) adjacent to it | whether the note is *true*; `unsafe impl`/`unsafe extern` declarations (their contract is a note on the impl, which the scan cannot separate from the surrounding code); a note that is correct for a neighbouring block but is placed within the run of comments above it |
| `unsafe-allow-outside-ffi` | all `crates/**` except `build.rs` | a file-level `#![allow(unsafe_code)]` outside the reviewed FFI/Vulkan locations | an outer `#[allow(unsafe_code)]` on a single item (out of scope by the rule text) |
| `missing-docs-lint` | every `crates/**/src/lib.rs` | a crate root that does not enable `#![warn(missing_docs)]` | a crate that enables the lint but still has undocumented items (that is `rustdoc`'s job) |

### Dropped tokens (with reasons)

Per the precision-over-recall rule, a token is dropped when a syntax scan
cannot decide it:

- **`Layer`** — Vulkan calls its validation layers "layer"
  (`VALIDATION_LAYER`, `VK_LAYER_KHRONOS_validation`), so the word is not
  TVP-specific. Flagging it would fire on the whole `render` crate.
- **`VK_` in `crates/platform/render`** — Vulkan's own `VK_*` constants share
  the prefix with the TVP virtual-key codes. The token is checked in every
  other L1 crate and in L2, where `VK_*` can only be the TVP table; in
  `render` it is dropped.
- **`.sli` / `.tjs`** — the extensions only occur inside string literals, and
  the mask blanks literals (a `Layer` inside a string is an explicit near
  miss). A token scan cannot see them without also flagging every filename in
  every test and log message.
- **KAG tag names** — `image`, `wait`, `eval`, … are ordinary English
  identifiers. Deciding one is a KAG tag needs the surrounding script, which
  the scan does not have.
- **bare `tjs`** — a local named `tjs` (a file list, a loop variable) is
  indistinguishable from the language name. `TJS` and `.tjs` are
  unambiguous; only `TJS` survives as a code token.
- **`Storage`** — L1 and L2 have their own storage types; the token is not
  TVP vocabulary.

### Deliberate exclusions

- `crates/compat/tests/**` from `l3-platform`: tests use `std::fs` for
  fixtures; that is not the compat *source*'s platform leak. No rule bans
  `std::fs` in a test file.
- `#[cfg(test)]` items from every rule, and `crates/*/tests/**` from the
  vocabulary and `unsafe` rules (`AGENTS.md` §5 says "outside tests").
- `crates/tjs/cpp/**` (vendored C++) and `build.rs` files.
- `crates/tjs/src/lib.rs` from `missing-docs-lint` is an **allowlisted
  exception**, not an exclusion: a peer owns the decision, and the allowlist
  entry references it rather than pre-empting it.

---

## 2. Self-test

`--self-test` runs before every scan (including the gated one), so a rule that
silently stops matching fails the gate. It scans `scripts/layering_fixtures/`
and asserts:

- one bad fixture per rule trips exactly that rule, and
- seven near-miss fixtures trip nothing: `Layer`/`TVP`/`KAG` in prose, a
  legitimate `Storage` type in L1, extensions inside literals, an `unsafe`
  block *with* its `SAFETY` note (adjacent and inline), an `unsafe fn` with a
  `# Safety` doc, a file-level allow inside the reviewed FFI boundary, and
  `std::fs` in a compat test.

```
$ bash scripts/check-layering.sh --self-test
layering guard: self-test ok (6 rules flagged their bad fixture, 7 near-miss file(s) clean, 7.2 ms)
```

Runtime: **self-test ~7 ms**, full scan **~1.3 s** (220 sites, 31 allowlist
entries), on the development host. The self-test prints its own runtime on
every invocation.

---

## 3. Triage

Run on the tree at `migration/middle-layer-ash` (2026-10-13):

| rule | findings | classification | disposition |
|---|---:|---|---|
| `l1-vocabulary` | 0 | clean | none |
| `l2-vocabulary` | 0 | clean | none |
| `l3-platform` | 25 | **25 real violations** of §2 (`std::fs` in compat source), in 6 files | known debt: allowlisted per file; the minimal fix is to route the operations through L1 `fs` / L2 `engine::Storage` once they expose them |
| `unsafe-no-safety-note` | 194 | **194 real §5 gaps**, in 24 files | known debt: allowlisted per file (the per-site list is long and churns); see `docs/audits/unsafe-inventory.md` §2.6, which already recorded that compat does not meet the rule |
| `unsafe-allow-outside-ffi` | 0 | all 88 file-level allows are in the four reviewed FFI/Vulkan locations | none |
| `missing-docs-lint` | 1 (`crates/tjs/src/lib.rs`) | acceptable / deferred | allowlisted, waiting on the crate-owner decision |

**Before/after:** 220 unallowlisted findings before the allowlist, 0 after,
with 31 reviewed entries (24 unsafe files, 6 `std::fs` file anchors, 1
`missing-docs` exception). The guard is green on the tree.

The `std::fs` sites, by file (all in `crates/compat/src/`):

| file | sites | what it emulates | missing L1/L2 operation |
|---|---:|---|---|
| `natives/plugins/fstat/mod.rs` | 16 | `TVP_stat`, `Storages` mutate/listing | metadata + write/copy/remove/read_dir/mkdir |
| `natives/plugins/json/mod.rs` | 3 | `JSON.save`/`load` | path read/write |
| `natives/plugins/save_struct/mod.rs` | 2 | `Array.saveStruct` | path read/write |
| `natives/plugins/layer_ex/draw.rs` | 2 | `LayerExDraw` record save/load | path read/write |
| `natives/plugins/psd_file/mod.rs` | 1 | PSD load | path read |
| `natives/debug.rs` | 1 | `Debug.startLogToFile` | `FileSystem` log sink |

These are real `AGENTS.md` §2 violations, not false positives. They are
recorded as **known debt** in the allowlist (marked `debt:`) rather than as
legitimate exceptions, because the fix is a tracked migration and not a
judgement that the code is correct.

The 194 `unsafe` gaps are the same shape: the `unsafe-inventory` already
classified them as required FFI whose contract is often documented once on a
helper, and already stated that the compat crate does not meet §5. The
allowlist baselines them per file so the guard still fails on a **new file** or
a **new rule**; it deliberately does **not** catch a new missing note inside an
already-listed file. That residual is the price of a green gate over a large
existing debt; a follow-up that adds the notes (or narrows the rule to the
genuinely undocumented sites) should shrink the allowlist.

---

## 4. Gate wiring

The guard belongs in the gate after the other compiler-free guards and before
`clippy`, matching the peer's house-style line. Exact lines (the main agent
applies them; `AGENTS.md`/`TODO.md` are owned by a peer):

`AGENTS.md` §0, after `bash scripts/check-house-style.sh`:

```
bash scripts/check-layering.sh                   # AGENTS §2/§5 guard
```

`TODO.md` §1, after `bash scripts/check-house-style.sh`:

```
bash scripts/check-layering.sh                                    # AGENTS §2/§5 guard
```

The allowlist is small enough to justify the gate: 31 entries, each with a
one-line reason, and stale entries are reported. No rule required hundreds of
allowlist entries. **No rule was dropped for volume**; the only tokens dropped
were ones a syntax scan cannot decide (see §1), with a stated reason.

---

## 5. Honest limits and the review backstop

1. **A token can resemble TVP vocabulary without being it.** The scan decides
   only spelling and position; it cannot decide intent. `TVP_`/`LT_`/`TJS`/
   `kag` are precise *because* they have no other meaning in L1/L2. Everything
   ambiguous (`Layer`, `VK_`, `Storage`, bare `tjs`) was dropped or scoped.
   The backstop is code review: a new L1/L2 type or function whose *name*
   encodes TVP semantics but does not use a listed token is not caught here.
2. **The scan cannot decide whether a `SAFETY` note is true**, only whether one
   is adjacent. The note's content is reviewed in
   `docs/audits/unsafe-inventory.md` §4; the guard only enforces presence.
3. **The note window is syntactic.** It accepts a note on the same line, on the
   first line inside the block, or in the contiguous comment/attribute run
   immediately above. A correct note placed after several lines of unrelated
   comments is missed (a false positive); that is the conservative direction.
4. **Baseline debt is invisible in the exit code.** A green run does not mean
   `AGENTS.md` §5 is met; 194 missing notes and 25 `std::fs` uses are recorded
   as debt. `--print-all` lists them. The `debt:` prefix in the allowlist is
   the marker a reviewer greps for.
5. **The `l3-platform` tests exclusion is a judgement.** A `std::fs` use that
   moves *out* of `tests/` into `src/` is flagged; the same call inside a test
   is not. The rule scope, not the call, is what is excluded.
6. **`crates/compat/tests/**` and `#[cfg(test)]` items are never checked for
   vocabulary or `unsafe` notes.** A test that hard-codes a TVP concept is
   acceptable fixture data; the rule is about the shipped surface.

The review backstop remains: the guard catches the mechanical regressions
(a token enters L1/L2, platform code enters compat, a file-level allow appears
outside the FFI boundary, a new crate skips the docs lint, a new file has an
undocumented `unsafe` block). Whether a matched token is *used* as TVP
knowledge, and whether a `SAFETY` note is correct, stay with the reviewer.

---

## 6. Reproducing

```bash
bash scripts/check-layering.sh --self-test          # ~7 ms
bash scripts/check-layering.sh --print-all          # 220 findings, no filter
bash scripts/check-layering.sh                      # gated, exit 0 on the tree
```
