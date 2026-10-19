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

`gate` runs the ordered sequence and includes the `cargo fmt`/`clippy`/`test`
steps, so it is for the main agent. `gate --no-cargo` runs only the
compiler-free checks. Every check also runs on its own; `android-target`
shells out to `cargo tree`, the rest read the tree.

## Layout

- `krkr_gates/` — the package; `cli.py` is the entry point.
- `krkr_gates/checks/` — one module per check, each exposing `run(argv) -> int`.
- `fixtures/` — the self-test inputs.
- `allowlists/` — the reviewed exceptions, one file per check.
- `pyproject.toml` / `uv.lock` — the project metadata.

## Checks

`stateless`, `error-handling`, `house-style`, `layering`,
`native-construction`, `lifecycle-order`, `host-facts`, `android-target`,
`surface-lock`, `native-parity`, `plugin-parity`, `native-arity`, the
`extract-xp3` helper, and `gate`.

`house-style` also enforces the AGENTS §6 comment rules: a **non-test**
comment may not cite the external engine (`reference/cpp`, a `*.cpp:line`, a
`.h`/`.hpp` filename, `C++`, `TVP`, `KiriKiri`, `ported`/`upstream`/`port of`)
or narrate history (`used to`, `previously`, `before this`, `replaced by`);
test comments keep those citations. In addition, no Rust file — tests
included — may carry `#[allow(clippy::…)]` / `#![allow(clippy::…)]` /
`#[expect(clippy::…)]`; the `clippy-allow` rule has no allowlist exemption.
Legacy comments that predate these rules are recorded in
`allowlists/house_style_baseline.txt` (JSON Lines keyed to the normalized
comment line, tracked in `TODO` §21); a stale baseline entry is a note, and
new citations outside the baseline still fail. `allowlists/house_style_allow.txt`
holds only reviewed false positives.

## Notes

- No third-party runtime dependencies; the build backend is hatchling.
- `native-parity` and `plugin-parity` regenerate their reports under `docs/`
  unless `--no-doc` is given.
- `surface-lock` regenerates the VM dispatch dump by running the compat
  surface test unless `--no-regen` is given; `--update` rewrites the committed
  baseline and is main-agent only.
