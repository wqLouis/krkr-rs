# AGENTS.md — working rules for krkr-rs

This is the contract for every human and AI agent that edits this repository.
It distills `docs/migration/**` (the design decisions are the source of truth
there) into the rules you must follow without re-reading them. If code and docs
disagree, the design docs win; fix the code, then update the doc.

Repo: `/mnt/DATA/Document/code/krkr-rs` · Branch: `migration/middle-layer-ash`
Real game for manual checks: `/mnt/DATA/Games/Others/test`.

---

## 0. The gate — the main agent runs it, nobody else

Only the **main agent** runs the build/test gate. Subagents (any delegated
worker) may **not** run `cargo build`, `cargo check`, `cargo clippy`,
`cargo test`, `cargo run`, or `rustc`. Parallel builds have exhausted `/tmp`
and corrupted the tree before. Subagents *may* run `cargo fmt` / `rustfmt`
(formatting is not a build) and may run already-built binaries under
`target/debug/`.

The gate runs in this order and must end green:

```bash
cargo fmt --all -- --check
bash scripts/check-stateless.sh                  # D9 guard
bash scripts/check-error-handling.sh             # AGENTS §4 guard
bash scripts/check-house-style.sh                # AGENTS §5/§6 guard
bash scripts/check-layering.sh                   # AGENTS §2/§5 guard
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast            # all crates; never fail-fast, so one
                                                 # failing target cannot hide the rest
cargo test -p platform --features platform-linux
cargo clippy -p platform --features platform-android --all-targets -- -D warnings
cargo test -p platform --features platform-android
python3 scripts/check-android-target.py              # per-target feature resolution
python3 scripts/surface_lock.py --update         # only the main agent; review the diff
python3 scripts/surface_lock.py
python3 scripts/native_parity.py
python3 scripts/plugin_parity.py --no-doc
```

`scripts/surface_lock.py --update` is for the **main agent only**, after the
change is understood.

### Releases are metered — push a tag only on the user's go-ahead

`.github/workflows/android.yml` triggers on `android-*` **tags** (a branch push
triggers nothing). Each tag runs a full Android build, which costs CI minutes,
so: commit locally, keep the gate green, and **hold the tag until the user says
the build is wanted**. Do not tag and push on every green gate. Batch the work,
report what is ready, and wait.

The `platform-android` steps build the **feature on the host**, not the Android
target: the feature compiles the platform-independent helpers plus a typed stub,
and everything that needs `ndk` stays gated to `target_os = "android"`. The
Android target itself is **never** built or tested here — there is no NDK on the
host and none may be installed (see §9 of `TODO.md`); it is compiled and run on a
machine that has one, and Android-only code that has not been compiled is
documented as unverified rather than described as working.

`scripts/check-android-target.py` resolves the dependency graph per target
without compiling anything and fails when a desktop-only backend feature leaks
into the Android target, or when two mutually-exclusive backends are enabled
together. A feature leak of that kind is invisible until something builds the
other way, which is how it reached the tree twice.

---

## 1. What this project is

A native Rust reimplementation of the KiriKiri2 (TVP) visual-novel engine,
running on Linux and Android. It is a **compatible emulator**, not a port: the
goal is that unmodified games and scripts behave exactly as they do on the
reference engine. `reference/cpp/**` is the oracle for behaviour.

---

## 2. The four layers and the one allowed dependency direction

```
L4  vm       TJS VM host (Tjs2Engine, GC, bytecode, script cache, frame body)
L3  compat   KiriKiri compatibility surface: native classes, KAG, event adapter,
             static plugins, Win32 emulation
L2  engine   a small, generic game engine: the layer system/scene graph, bitmaps,
             sound mixer, timers, VFS namespace; owns engine state
L1  platform api · fs · media · render · platform
             stateless services + all platform code (window, event loop, input,
             clock, threads, Vulkan, FFmpeg/MediaCodec, raw IO + XP3 parse)
app          composition root: select platform, wire L1..L4, run the loop
```

Dependency direction (strict, one extra edge):

```
L4 vm      → compat(L3), platform(L1)
L3 compat  → engine(L2), platform(L1), api, fs, tjs
L2 engine  → L1 type crates (api, fs, render)
L1         → OS / Vulkan / FFmpeg / MediaCodec only
app        → everything (wiring only)
```

Hard rules:
- **L1 must not know about layers, TVP, TJS, storages or KAG.**
- **L2 must not contain TVP/TJS vocabulary** (no `LT_*`, `TVP_*`, VK codes,
  `.sli`, the `0..100000` volume scale, KAG tags). Express generic concepts
  (`BlendMode`, normalized gain, opaque key codes).
- **L3 must not contain platform code** (`#[cfg(target_os = …)]`, `winit`,
  `android-activity`, `std::fs`, JNI). It uses L1 abstractions.
- **`#[cfg(target_os)]` and platform crates live only in L1 (`platform`, `media`).**

Crate roles:
| crate | layer | role |
|---|---|---|
| `tjs` | vendored | C++ TJS2 VM FFI (unchanged, all-FFI) |
| `api` | L1 | shared DTOs + typed `*Error` enums, no impls |
| `fs` | L1 | stateless `FileSystem`/`FileState` + XP3 read/parse |
| `media` | L1 | stateless `Media` + image/audio/video decode |
| `render` | L1 | stateless `RenderPipeline`/`FontSystem` + Vulkan |
| `platform` | L1 | window/surface, event loop, input, clock, threads |
| `engine` | L2 | scene graph, mixer, timers, VFS, projection |
| `compat` | L3 | native classes, KAG, event adapter, plugins |
| `vm` | L4 | VM host |
| `app` | root | thin binary: wiring only, no platform logic |
| `testkit` | dev | shared test helpers, dev-dependencies only |

---

## 3. The non-negotiable decisions (D-rules)

These are recorded in `docs/migration/README.md` §3. The **code must not cite
them by number** (§6); this section is for you to know them.

- **D9 — L1 is stateless.** L1 objects are facades over platform capabilities;
  methods are `&self` and take evolving state as an explicit `&mut` value
  (`RenderState`, `FileState`, `DecoderState`, …). No `static mut`,
  `thread_local!`, `LazyLock`/`OnceLock`/`OnceCell`, no process-global
  registries, no hidden caches, no `&mut self` on an L1 service. All engine
  state is owned by L2. Guarded by `scripts/check-stateless.sh`.
- **D10 — L1 owns traversal/rendering.** L2 maintains the `RenderScene` tree and
  calls `render`; it never builds a draw list. L1 does traversal, composition,
  batching, blend selection and drawing.
- **D11 — no silent fallbacks, no hardcoded limits.** Where the intended
  behaviour cannot be performed (unsupported feature, malformed/overflowing
  data, unrepresentable size), return a **typed error**. No degraded
  substitutes, no arbitrary size caps, no "guess and continue", no
  `unwrap_or`/`_ =>` that hides a failure. The **one** documented exception is
  the null-plugin object (§D14 below), which logs a warning on every call.
- **D14 — native plugins are static, built into L3.** One `BUILTIN_PLUGINS`
  table; `Plugins.link(name)` recognizes every ported DLL. A DLL we have **not**
  ported gets the null-plugin object: accepts every member/call, returns
  void/undefined, logs `warn!` per call, never throws. This is not silent.
- **D16 — L2 stays generic; L3 stays thin.** Every L3 native member should
  reduce to *marshal args → one L2 call → marshal result*. Real logic (loops,
  state machines, blending, timing, scheduling) belongs in L2 unless it is
  genuinely TVP-specific. Do **not** push TVP specifics into L2 to shrink L3;
  do **not** grow L3 into a second engine.
- **D18 — emulate the Win32 environment.** Games are Windows-native and probe
  Win32 APIs. `HWND`, draw devices, message receivers, registry, shell,
  system metrics must return **emulated** values/handles (synthetic stable
  integers), never typed errors. A typed error there is a compatibility bug.
- **D26 (in progress) — L2 owns the frame.** `Engine::frame(now_ms, hooks)`
  advances timers/mixer/video, runs `GameFrame::before_render()`, projects and
  renders. Nothing above L2 owns a `dt`. Until it lands, `compat` orchestrates
  and `platform::Clock` supplies wall time; keep deterministic test clocks.
- **D27 — the L2 timer is `create_interval`/`set_interval`/`set_enabled`/`remove`.**
  No config struct, no mode enum, no fire list to hand upward.
- **D28 — no game-specific code.** A behaviour change must implement what
  `reference/cpp/**` does. Never special-case one title to make it work.
- **D29 — the surface lock is the contract.** `crates/compat/tests/runtime_surface.rs`
  dumps what `CompatHost::install` actually registered; `scripts/surface_lock.py`
  fails on any added/removed member. L3 internals are free to change only while
  the dump is unchanged. Adding a member needs a reviewed baseline update.
- **D30 — tests cite the reference; the code does not.** Every behaviour
  assertion carries a `reference/cpp/...:line` citation. Implementations owe no
  citation and no resemblance to the C++.
- **D31 — comments describe the code as it is.** No history, no "used to",
  no design rationale, no decision numbers (`D9`, `D31`, …), no cross-links to
  docs. Keep `// SAFETY:`, non-obvious invariants, and reference citations
  **in tests**. Tests are exempt from the jargon rule.
- **D32 — only the Android backend may be stubbed.** L3 has no stubs and no
  *script-observable* approximations: every member either behaves like the
  reference or raises because the reference raises. Android is the one backend
  no script can observe. `#[allow(dead_code)]` is banned (one exception:
  `crates/compat/tests/common/mod.rs`).
- **D33 — L1 may change the `engine` signatures it names.** `engine` depends on
  the L1 type crates (`api`, `fs`, `render`) and its public API may use their
  types (`RenderScene`, `FileState`, `FontMetrics`, …). An L1 type change that
  flows into an `engine` signature is accepted. The L2 surface is frozen by
  review and audit, not by a snapshot (the `cargo public-api` guard is dropped).
- **D34 — rendering picks the more efficient path when it looks near-identical;
  script-observable semantics stay exactly reference.** Pixel rasterization
  (hatch/gradient fills, GPU composition, transitions) may approximate
  efficiently if the visible output is close. Every value a script can observe
  — member getters/setters, return values, raised errors, `typeof`, string and
  number conversions — must match `reference/cpp/**` exactly. This is the
  boundary of D32's ban.

---

## 4. Errors, fallbacks and "slop" — what to block

The single most important review criterion is: **does this change introduce a
silent fallback?** Block and fix any of these:

- A wildcard `_ =>` arm that swallows a newly-added enum variant instead of
  handling it (D29/D30: prefer an exhaustive match so a new variant is a
  compile error). Enums L3 matches variant-by-variant must **not** be
  `#[non_exhaustive]`.
- `unwrap_or(default)` / `unwrap_or_default()` / `unwrap_or_else` on a `Result`
  that could carry a real error (I/O, decode, parse, out-of-range).
- `if let Ok(x) = …` / `.ok()` / `.ok()?` that drops the error variant.
- `let _ = result;` on a fallible call.
- A function that returns `Option`/`bool` where `NotFound` and a genuine error
  must be distinguishable (e.g. storage `find`); make it `Result<Option<…>>`.
- Any numeric cap with no reference basis (`> 256`, `MAX_… = 1024`) — see D11.
  Use `checked_*`/`try_from` and a structural validation error instead.
- An arithmetic cast (`as usize`, `as u32`) on untrusted/script data that can
  truncate; use `try_from` or saturating arithmetic.
- `debug_assert!` guarding a data-integrity condition that must hold in
  release; make it a real typed error.
- A background thread that panics or a lock `expect` reachable from malformed
  input.
- `#[allow(dead_code)]`, `#[allow(unused)]` (D32). If it is dead, delete it.
- A stub `todo!`/`unimplemented!`/approximation in L3 (D32). Android-only stubs
  are the sole exception.

Typed errors: one `#[non_exhaustive]` error enum per domain
(`FsError`, `MediaError`, `RenderError`, `GlyphError`, `AudioError`,
`TextError`, `SoundError`, `TimerError`, `VideoError`, `TransError`, …), owned
by the crate that declares it. Errors are values; no panics on bad input.

---

## 5. Idiomatic Rust (this codebase's house style)

- **No OOP bean accessors.** A setter whose body is only `self.field = arg`
  must be a public field. A getter for a public field is deleted. Accessors
  exist only to enforce an invariant or do real work, and are named for the
  work (`clamp_opacity`, `set_interval`), never `get_foo`/`set_foo` when they
  only move a field. Getters that read a real property are `foo()`, not
  `get_foo()`.
- Prefer explicit values and `&self` actions; avoid hidden `&mut self` state on
  service objects.
- Parameters: `&str`, `&[T]`, `&mut [T]`, `impl Into<…>`; never `&String`,
  `&Vec<T>`, or `&Box<T>`.
- Use `TryFrom`/`checked_*` for narrowing and size math; `From`/`into` for
  widening.
- Derive what you can (`Default`, `Copy`, `Eq`) and only implement manually
  with reason. `Eq` requires no floats.
- Keep public API docs on every public item (`#![warn(missing_docs)]`).
- Re-export deliberately; a flat glob re-export of the whole crate is not the
  default (freeze only the intended contract).
- `unsafe` is forbidden in pure crates (`api`, `fs`, `engine`, `testkit` and
  the pure modules) and `deny`ed at the crate root elsewhere, with a
  file-level `#![allow(unsafe_code)]` only where the FFI/Vulkan truly lives.
  Every `unsafe` block carries a `// SAFETY:` note.
- No new workspace dependency without a reason in the change description
  (performance work is encouraged only where a reference test still holds;
  D30).

---

## 6. Comments, docs, and naming

- **D31:** a comment says what the code does now. Delete "was", "used to",
  "previously", "replaced by", design rationale, and decision numbers. History
  is read with `git`.
- No internal jargon in **printed/logged** text (`L1`–`L4`, decision numbers).
  Use `platform`, `engine`, `compat`, `app`, `tjs`, `vm`, or plain English.
  Tests are exempt.
- Reference citations belong in tests, formatted `reference/cpp/...:line`.
- A test without a reference citation above the behaviour assertion is not
  evidence; do not add one.
- Name things exactly as the reference where it is script-visible (member
  names, error strings); use generic names in L2.

---

## 7. What belongs where (the genericity test)

For any behaviour, ask: *does it need TVP vocabulary?*

- Generic scene graph, bitmaps, mixing, timers, VFS, projection → **L2**.
- `arc.xp3>path`, masks, province, `Layer.type`, focus/hit-test, `Font.face`,
  KAG tags, `.sli`, the TVP volume scale, VK codes → **L3**.
- Platform host, window, event loop, raw IO, decode, Vulkan → **L1**.

Two-sided tests:
- If moving a behaviour into L2 requires naming a TVP concept, it stays in L3.
- If an L3 file is larger than the L2 module it wraps, the abstraction is
  leaking; push generic behaviour down (or the shared behavior struct into L2).
- Shared behaviour is a **struct composed** by several thin L3 classes, never
  a registration list copied per class.

---

## 8. Tests

- Tests live in `tests/` (integration) or `#[cfg(test)]` modules, and cite
  `reference/cpp/...:line` for every behaviour assertion.
- A regression fix gets a test that would have failed before it.
- Prefer driving the real public surface (the VM, `Env::eval`) over hand-built
  intermediates for L3 behaviour.
- Keep the surface dump and parity scripts green; a change that adds/removes a
  registered member is a reviewed baseline change, not a silent edit.
- Known harness helpers exist in `crates/compat/tests/common/mod.rs`
  (`eval_then` for write-then-read, `non_primary_layer`, etc.) — use them.

---

## 9. Known TJS/VM traps (these have all cost real time)

1. `typeof` uses **TJS spellings**: `"Object"`, `"Integer"`, `"Real"`,
   `"String"`, `"Octet"`, `"void"`. Only an *absent* member yields
   `"undefined"`; a present-but-unset member is `"void"`.
2. `void` is not `undefined`; a bare `undefined` raises.
3. `property` is a keyword — `var property = …` is a syntax error.
4. Inside a subclass body a bare native class name resolves to `this.<Name>`;
   write `new global.Layer(…)`.
5. `Env::eval` compiles in expression mode; write-then-read must be separate
   executions (use `eval_then`).
6. The first layer created on a window is force-visible.
7. The compat binding is process-global and single: drop the first `Env`
   before creating a second in one test.
8. A dictionary swallows a missing read as `void`; a custom object reports
   absence. `get_member(...).is_err()` cannot detect a missing dictionary key;
   use `typeof o[k]`.
9. Vendored C++ can be a reverse-engineered reconstruction and can contradict
   the shipped data; when it does, say so explicitly with the evidence.
10. TJS2 does not bind `function` declarations inside an anonymous function
    body (no IIFE wrappers); use a named function, call it, delete it.
11. `Layer.id`/`nativeId`, `Bitmap.id`, `Window.id`, `Font.id` do not exist by
    design; identify through real members or the L2 `Scene`.
12. Loading is explicit (`--archive`, repeatable; later wins). There is **no** folder scan in the reference (D15), but D35 adds one opt-in exception: a directory-only launch (`--base-dir`/positional dir, no `--archive`) mounts every `*.xp3` found recursively (`engine::storage::discover_archives`).

---

## 10. Definition of done for a change

1. Behaviour matches `reference/cpp/**` (or raises exactly where it does), and
   every behaviour test cites it.
2. No silent fallback, no arbitrary cap, no `#[allow(dead_code)]`, no L3 stub
   or approximation.
3. Layer and dependency rules hold (no platform code in L3, no TVP vocabulary
   in L2, no L2/L3 knowledge in L1).
4. The surface lock and both parity reports are unchanged (or the baseline
   change is deliberate and reviewed).
5. Comments/D31 clean; public items documented; `cargo fmt` clean.
6. The main agent's full gate is green.

---

## 11. Reference pointers (read on demand, do not re-derive)

| Topic | Doc |
|---|---|
| Decisions + rationale | `docs/migration/README.md` §3 |
| Layers, crate layout, acceptance criteria | `docs/migration/02-target-architecture.md` |
| L1/L2 stateless-object contract | `docs/migration/03-middle-layer-api.md` |
| Vulkan pipeline, blend mapping | `docs/migration/04-renderer-vulkan.md` |
| Media engine | `docs/migration/05-media-engine.md` |
| File system | `docs/migration/06-file-system.md` |
| Roadmap + phase 9 (L2 owns the frame) | `docs/migration/07-roadmap.md` |
| L3/L4, plugins, null plugin, ABI | `docs/migration/08-tjs-vm-and-native-plugins.md` |
| Game loading, precedence, startup order | `docs/migration/09-game-loading.md` |
| Android AAudio/MediaCodec | `docs/migration/10-android-backends.md`, `docs/android.md` |
| Per-crate review findings | `docs/migration/reviews/*.md` |
| Surface lock / parity | `docs/native_parity.md`, `docs/plugin_parity.md` |
| Win32/plugin-host and Wine studies (non-goals) | `docs/plugin_host_exe.md`, `docs/wine_box64.md` |
