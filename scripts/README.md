# Gate checks (`krkr-gates`)

The repository's mechanical gate checks live here as one uv project. Its
console entry point is `krkr-gate`.

## Run

From the repository root:

```bash
uv run --project scripts krkr-gate gate              # full gate (main agent; includes cargo)
uv run --project scripts krkr-gate <check>           # one check
uv run --project scripts krkr-gate <check> --self-test
```

`gate` runs the ordered sequence and includes the `cargo fmt`/`clippy`
steps, so it is for the main agent; the tests run separately with
`krkr-gate test`. `gate --no-cargo` runs only the
compiler-free checks. Every check also runs on its own; `android-target`
shells out to `cargo tree`, the rest read the tree.

The static gate sequence is `cargo fmt` → `stateless` → `error-handling` →
`house-style` → `layering` → `storage-boundary` → `native-construction` →
`lifecycle-order` → `storage-media` → `callback-objthis` → `native-arity` →
`void-getter` → `native-instance-lookup` → `cargo clippy` → `cargo clippy platform-android` →
`android-target` → `surface-lock --update` → `surface-lock` → `native-parity`
→ `plugin-parity --no-doc` → `host-facts` → `ci`. The ordered gate, the test
suites and the CI release pipeline are documented in
[`../docs/rules/gates.md`](../docs/rules/gates.md).

## Layout

- `krkr_gates/` — the package; `cli.py` is the entry point.
- `krkr_gates/checks/` — one module per check, each exposing `run(argv) -> int`.
- `fixtures/` — the checks' own test corpus; see below.
- `allowlists/` — the reviewed exceptions, one file per check.
- `pyproject.toml` / `uv.lock` — the project metadata.

### The fixtures are the checks' own tests

Every check carries a `--self-test` that runs it against a small corpus of
deliberately broken and deliberately clean inputs, so a check that stops
detecting anything fails the gate instead of passing it silently. The corpus
lives in `fixtures/`, one directory per check:

- a `bad_*` file is a case the check **must** report — a pointer cast without
  a safety note, an error swallowed by `unwrap_or`, a layer that reaches into
  another layer's engine state;
- a `good_*` file is a case the check **must not** report — the same code with
  the problem fixed;
- three checks (`storage-boundary`, `storage-media`, `host-facts`) scan a whole
  miniature source tree under `fixtures/<check>/tree/` and compare what they
  find against an `expected.json`, so a file there is a *sample* of the code
  the check reads, not a file it names individually.

They are therefore not a reference copy of anything: they are the inputs of
the checks' self-tests, and the gate exercises them on every run
(`check-host-facts: self-test ok (4 seam(s), 2 unwired in the fixture)`).
Deleting one makes that check's self-test fail.

## Checks

`stateless`, `error-handling`, `house-style`, `layering`,
`storage-boundary`, `native-construction`, `lifecycle-order`, `storage-media`,
`callback-objthis`, `native-arity`, `void-getter`, `native-instance-lookup`,
`host-facts`, `android-target`, `ci`, `surface-lock`, `native-parity`,
`plugin-parity`, the `extract-xp3` helper, and `gate`.

`house-style` also enforces the AGENTS §6 comment rules: a **non-test**
comment may not cite the external engine (`reference/cpp`, a `*.cpp:line`, a
`.h`/`.hpp` filename, `C++`, `TVP`, `KiriKiri`, `ported`/`upstream`/`port of`)
or narrate history (`used to`, `previously`, `before this`, `replaced by`);
test comments keep those citations. In addition, no Rust file — tests
included — may carry `#[allow(clippy::…)]` / `#![allow(clippy::…)]` /
`#[expect(clippy::…)]`; the `clippy-allow` rule has no allowlist exemption.
Legacy comments that predate these rules are recorded in
`allowlists/house_style_baseline.txt` (JSON Lines keyed to the normalized
comment line, tracked in `TODO` §15); a stale baseline entry is a note, and
new citations outside the baseline still fail. `allowlists/house_style_allow.txt`
holds only reviewed false positives.

`layering` enforces the layer rules that a compiler cannot: L1 crates may not
name TVP/TJS/KAG vocabulary, L2 may not carry TVP vocabulary or storage ids,
L3 may not contain platform code, and L3 reaches the engine only through
`crates/compat/src/engine_api.rs` (the `l3-engine-api` rule). It also checks
that every `unsafe` block carries a `// SAFETY:` note and that the file-level
`unsafe_code` allow appears only in reviewed FFI files.

`ci` statically validates the release workflow contract: the `v*` tag
trigger (and the absence of any `android-*` tag), a build-only workflow that
does not rerun the local static gate, independent Linux and Android build jobs,
the Linux archive, the Android build/packaging scripts, and a single final
release job that attaches both artifacts. It reads the workflow as text
(standard library only) and has a fixture self-test under `fixtures/ci/` that
proves every rule fires.

`void-getter` fails a native property getter that returns a `StoredValue`
field whose constructor default is `StoredValue::Void` unchanged: the script
receives a void value, and reading a member from it raises, where the
reference returns an array, a dictionary or a native object. A getter that
matches the void default and returns the reference's non-void value is clean.
A property whose reference getter genuinely reads back void is recorded in
`allowlists/void_getter_allow.txt` as `path:field — reason`; a reason that
starts with `debt:` tracks a known defect and does not silence the finding.

`native-instance-lookup` guards the native-instance ABI seam in
`crates/tjs/cpp/tjs2_abi.cpp`: `tjs2_get_native_instance` must resolve a
live-but-invalidated instance through `GetNativePtrForLookup()`, that accessor
must stay the valid-only class lookup, and the four method/property/constructor
dispatch bodies must keep using the invalidation-guarded `GetNativePtr()`.
Its fixture self-test applies four mutations and requires the matching rule to
fire.

## Environment

- `KRKR_TEST_GAME` — a game directory for the `real_game`/`boot_game` tests.
- `KRKR_TEST_SAVE` — a real KAG save BMP for the save-fixture test.
- `KRKR_REPO` — the checkout root; docs use it where a command starts outside
  the repository.
- `KRKR_REFERENCE` — the reference checkout the parity checks read (a clone
  with a `cpp/` directory). Defaults to `<repo>/reference`;
  `krkr-gate --reference PATH <check>` sets it for that run, and
  `krkr-gate --reference PATH gate` hands it to every check the gate runs.
  Without it, a clone at another path makes `native-parity` and
  `plugin-parity` fail with the paths they tried instead of scanning nothing.

## Notes

- No third-party runtime dependencies; the build backend is hatchling.
- `native-parity` and `plugin-parity` regenerate their reports under `docs/`
  unless `--no-doc` is given; the reports cite reference files as
  `reference/cpp/...` whatever path the checkout actually lives at.
- `surface-lock` regenerates the VM dispatch dump by running the compat
  surface test unless `--no-regen` is given; `--update` rewrites the committed
  baseline and is main-agent only.
