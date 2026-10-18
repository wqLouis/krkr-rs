This project's agent instructions live in `AGENTS.md` (the name the agent harness
auto-loads). Read that file first; this one is only a pointer so the singular
spelling also resolves.

Agent baseline:

1. `AGENTS.md` — roles, architecture, decisions, layer rules, the gate and the
definition of done. The main agent orchestrates and integrates; subagents get
narrow, disjoint, independently reviewable scopes.
2. `docs/code-conduct.md` — the Rust-level baseline for what an agent writes:
naming, signatures, types, traits/macros, comments, and the worker report.

See [AGENTS.md](AGENTS.md) and [docs/code-conduct.md](docs/code-conduct.md).
