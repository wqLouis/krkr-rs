# The documentation tree

This tree documents `krkr-rs` — a native Rust reimplementation of the KiriKiri2
(TVP) visual-novel engine, running on Linux and Android.

The tree answers three different questions, and each kind of page answers
exactly one of them:

| Question | Directory | Page shape |
|---|---|---|
| *What is the design?* — the layers, the crate map, the coordinate spaces, the frame, the L3↔L2 protocol | [`arch/`](arch/overall.md) | the contract, stated once |
| *What does the code do?* — one page per module, with its API, invariants, data flow and failures | [`code/`](code/README.md) | the code map |
| *What must a change obey?* — the layering, state, frame, error and Rust-style rules | [`rules/`](rules/arch_rules.md) | the review checklist |

Start with [`arch/overall.md`](arch/overall.md) if the architecture is new to
you, [`code/README.md`](code/README.md) if you are looking for a module, and
[`rules/arch_rules.md`](rules/arch_rules.md) if you are about to change
something.

The working contract for every agent and maintainer that edits this repository
is [`../AGENTS.md`](../AGENTS.md). It is the short form of the three rule pages
above and adds the process rules (the gate, the roles, the release metering).

---

## 1. The architecture

| Page | Covers |
|---|---|
| [`arch/overall.md`](arch/overall.md) | the four layers, the dependency direction, the crate map, the coordinate spaces, the frame |
| [`arch/l1.md`](arch/l1.md) | L1 — the stateless platform core: the services, the state values, the object/state split |
| [`arch/l2.md`](arch/l2.md) | L2 — the engine: the layer tree, the projection, the storage namespace, the frame |
| [`arch/l2-api.md`](arch/l2-api.md) | the L2 public API: the engine crate's types, operations, error types and stability contract |
| [`arch/l3.md`](arch/l3.md) | L3 — the compatibility surface: the native classes, the plugins, the events, the frame body |
| [`arch/l4.md`](arch/l4.md) | L4 — the TJS VM host: the engine wrapper, the script cache, the vendored VM |
| [`arch/embedding.md`](arch/embedding.md) | embedding the engine and the platform services in another application: the crates, the seams, the frame contract, the script layer |

---

## 2. The rules

| Page | Covers |
|---|---|
| [`rules/arch_rules.md`](rules/arch_rules.md) | the layering, the state ownership, the frame contract, the coordinate spaces, the typed errors, the backend selection |
| [`rules/api_rules.md`](rules/api_rules.md) | the public surfaces, the L3↔L2 protocol, the ownership rules, the error enums |
| [`rules/code_styles.md`](rules/code_styles.md) | the Rust-level review checklist |
| [`rules/gates.md`](rules/gates.md) | the ordered static gate, the test suites, the CI release pipeline, the `v*` tag convention, the vendored TJS2 patch |

---

## 3. The code map

[`code/README.md`](code/README.md) indexes every module page, grouped by
crate:

| Directory | Crates | Layer |
|---|---|---|
| [`code/platform/`](code/platform/README.md) | `api`, `fs`, `media`, `psd`, `render`, `platform` | L1 |
| [`code/engine/`](code/engine/README.md) | `engine` | L2 |
| [`code/compat/`](code/compat/README.md) | `compat` | L3 |
| [`code/vm.md`](code/vm.md), [`code/tjs.md`](code/tjs.md) | `vm`, `tjs` | L4 |
| [`code/app/`](code/app/README.md) | `app` | root (desktop) |
| [`code/android/`](code/android/README.md) | `android` | root (Android) |
| [`code/testkit.md`](code/testkit.md) | `testkit` | dev-only |

---

## 4. The generated reports

These three files are **generated** or **machine-read**; their paths are
hard-coded in the gate scripts, so they stay at the top level of `docs/`.

| File | What it is | Produced by |
|---|---|---|
| [`native_parity.md`](native_parity.md) | the core-class surface parity report against the reference | `krkr-gate native-parity` |
| [`plugin_parity.md`](plugin_parity.md) | the plugin surface parity report against the reference | `krkr-gate plugin-parity` |
| [`surface/runtime_surface.json`](surface/runtime_surface.json) | the **surface lock**: what `CompatHost::install` actually registered | `krkr-gate surface-lock --update` |

Do not hand-edit them. `surface/runtime_surface.json` is the lock: a change to
a registered member fails the gate until the baseline is deliberately updated
and reviewed.

---

## 5. The archive

[`../docs.old/`](../docs.old/) is the **superseded** tree: the previous
top-level pages, the design record and the read-only audits. Nothing there is
authoritative any more — the pages above are.

It is kept as the **evidence archive**, for two reasons:

* the decision log (the D-rules) and the deep reference evidence trail live
  there, and a page that cites a decision cites its archived page;
* the audits (the error and `unsafe` inventories, the storage layering, the
  `VideoOverlay` behaviour, the perf and memory measurements) record the state
  of the tree at the time they were written — they are history, not contract.

| Archived group | Contents |
|---|---|
| `docs.old/migration/` | the design record: the decision log and the per-topic design documents |
| `docs.old/audits/` | the read-only audits (17 pages) |
| `docs.old/android.md` | the archived Android launcher guide |
| `docs.old/runtime-config.md` | the archived runtime configuration surface |
| `docs.old/code-conduct.md` | the archived Rust baseline, replaced by `rules/code_styles.md` |
| `docs.old/performance.md`, `docs.old/optimization.md` | the measured performance work |
| `docs.old/plugin_host_exe.md`, `docs.old/wine_box64.md`, `docs.old/vm-defects.md`, `docs.old/gdiplus.md`, `docs.old/fonts.md`, `docs.old/font_crates.md`, `docs.old/portability.md`, `docs.old/startup-attribution.md` | the reference studies and the notes |

The current contract for a topic always lives under `arch/`, `code/` or
`rules/`; an archived page is cited only as the evidence for a decision.
