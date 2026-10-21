This project's agent instructions live in `AGENTS.md` (the name the agent harness
auto-loads). Read that file first; this one is only a pointer so the singular
spelling also resolves.

Agent baseline:

1. `AGENTS.md` — roles, architecture, decisions, layer rules, the gate and the
definition of done. The main agent orchestrates and integrates; subagents get
narrow, disjoint, independently reviewable scopes. §0.1 defines the main
agent's loop — set direction, have workers deliver `/tmp/<task>.patch` from an
isolated copy, review/apply the patch, run the gate, measure, integrate — and
§0.4 defines the gate only the main agent runs.
2. `docs/rules/code_styles.md` — the Rust-level baseline for what an agent writes:
   naming, signatures, types, traits/macros, comments, and the worker report.
   (`docs/rules/arch_rules.md` and `docs/rules/api_rules.md` carry the layer,
   state, frame and public-surface rules; `docs/code/README.md` is the module
   map. The superseded design record and the audits are archived in
   `docs.old/`.)

See [AGENTS.md](AGENTS.md) and [docs/rules/code_styles.md](docs/rules/code_styles.md).
