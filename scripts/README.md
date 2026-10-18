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
`surface-lock`, `native-parity`, `plugin-parity`, the `extract-xp3` helper,
and `gate`.

## Notes

- No third-party runtime dependencies; the build backend is hatchling.
- `native-parity` and `plugin-parity` regenerate their reports under `docs/`
  unless `--no-doc` is given.
- `surface-lock` regenerates the VM dispatch dump by running the compat
  surface test unless `--no-regen` is given; `--update` rewrites the committed
  baseline and is main-agent only.
