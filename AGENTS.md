# AGENTS.md — the working contract for krkr-rs

This is the contract for every human and AI agent that edits this repository.
It is the short form of the design record that lives here — the architecture
in `docs/arch/`, the obligations in `docs/rules/`, the code map in
`docs/code/` — plus the process rules: how a change is made, reviewed and
integrated, and what the gate demands before it is committed. If the code and
the current docs disagree, the current docs win; fix the code, then update
the doc.

Read order: this file, then `docs/rules/code_styles.md` for the Rust-level
baseline, then only the pages a task actually needs — the architecture in
`docs/arch/`, the module map in `docs/code/`, the rules in `docs/rules/`.
Do not load the whole repository into every context.

---

## 0. How a change is made here

One integrator owns a change end to end, and may build it in parallel with
workers on disjoint scopes. The rules below are how this repository is worked
on, whoever does the work.

### 0.1 Roles — the integrator and the workers

The **integrator** (the main agent, or the human who owns the change) holds
the whole picture: the layer model, the design decisions, the surface lock,
the gate state. They decide *what* changes, *where* it belongs and *whether*
it is correct, and they integrate it into the tree; they do not spend their
context typing every mechanical edit.

The **worker** (a subagent) implements one coherent change in a narrow scope.
They do not redesign the surrounding system, fix unrelated bugs, or change
behaviour they were not asked to change; a bug they find outside their scope
is reported, not fixed.

User-visible design, cross-layer changes, the surface baseline, commits, tags
and the final gate stay with the integrator. Routine module cleanup, a scoped
bug fix and a mechanical migration are delegated.

The integrator can always answer: which layer owns this behaviour, which
reference behaviour is the oracle, what is in flight, and where the risk is.

### 0.2 The integrator's loop

1. **Set direction.** Name the module, the exact files, the reference lines
   and the outcome. The brief says *what* and *where*; the worker works out
   the *how*.
2. **Dispatch disjoint workers.** One coherent change per worker, and no two
   workers writing the same file in one wave. Work is produced in an isolated
   `/tmp/<task>-work` copy or worktree and delivered as `/tmp/<task>.patch`:
   the worker commits a baseline there, edits, and runs
   `git diff > /tmp/<task>.patch`. A worker never commits or pushes the real
   checkout, so the tree is never left half-edited.
3. **Verify the patch.** Read the diff, apply it with `git apply --3way`,
   resolve conflicts, and check the layer boundary, the reference behaviour,
   the surface lock, the task's non-goals and silent fallbacks.
4. **Run the gate.** Only the integrator runs `cargo fmt`, clippy, the full
   static gate and both test suites (§0.4).
5. **Measure.** Re-run the trace or benchmark that motivated the change; a
   performance claim is only done after a symbol-rich release measurement.
6. **Integrate.** Delete the superseded path or doc, update the design record
   and commit only after the gate and the tests are green. A worker's report
   is a proposal, not a completed change.

**Deletion is part of the change.** When a patch replaces an implementation,
test, allowlist entry or doc, the superseded one is deleted in the same patch:
no compatibility shim, no stubbed old signature, no "kept for reference" copy,
no appended "formerly" section. A patch that adds a path without removing the
one it replaces is rejected, and so is a doc that appends a superseded
section instead of replacing it.

### 0.3 A worker's brief

Every worker brief states five things, and is short enough to paste:

```text
Goal:        one behavior or cleanup, with the evidence that proves it
Write:       exact files/directories inside the isolated copy; no other writes
Work:        /tmp/<task>-work; deliver /tmp/<task>.patch; no repo commit
Read:        the module, its callers, and the cited reference lines
Must hold:   invariants, public/script surface, layer boundary
Do not:      unrelated fixes, new deps, public renames, commits
Verify:      rustfmt + compiler-free guards only; main runs the gate
Report:      files, patterns, risk, left alone, blockers
```

Parallel workers only run on **disjoint write scopes**; two workers never edit
the same file in one wave. They must not run `cargo build`, `cargo check`,
`cargo clippy`, `cargo test`, `cargo run` or `rustc` — parallel builds have
exhausted the machine's disk and corrupted the tree before. They *may* run
`cargo fmt` / `rustfmt` (formatting is not a build), the compiler-free gate
checks (§0.4) and already-built binaries under `target/debug/`.

When a result arrives, the integrator reads the patch, not only the report: it
checks the layer boundary, the reference behaviour, the surface lock and the
task's non-goals, and rejects silent fallbacks, unrelated refactors, new
dependencies, unreviewed public/script changes, an added path that leaves the
superseded one in place, and a doc that appends a superseded section. The
patch is applied with `git apply --check` and then `git apply --3way`; a patch
with conflict markers is never accepted.

### 0.4 The gate — the integrator runs it, nobody else

The gate runs in one command and must end green:

```bash
uv run --project scripts krkr-gate gate
```

It runs the ordered **static** gate: `cargo fmt --all -- --check`; the
`stateless`, `error-handling`, `house-style`, `layering`,
`storage-boundary`, `native-construction`, `lifecycle-order`,
`storage-media`, `callback-objthis`, `native-arity`, `void-getter` and
`native-instance-lookup` checks;
`cargo clippy --workspace --all-targets -- -D warnings`;
`cargo clippy -p platform --features platform-android --all-targets -- -D warnings`;
`android-target`; `surface-lock --update`; `surface-lock`; `native-parity`;
`plugin-parity --no-doc`; `host-facts`; `ci`.

The static gate does **not** run the test suites. Run them separately:

```bash
uv run --project scripts krkr-gate test
```

which runs `cargo test --workspace --no-fail-fast`, then the `platform`
suites for `platform-linux` and `platform-android`. A change is not done
until both the static gate and the test suites are green.

Every check also runs on its own (`krkr-gate <check>`) and takes
`--self-test`; `gate --no-cargo` runs only the compiler-free checks, and
`gate --keep-going` reports every failure instead of stopping at the first
one. `surface-lock --update` rewrites the committed baseline and is for the
integrator only. The ordered gate, every check's contract and the CI release
pipeline are documented once in
[`docs/rules/gates.md`](docs/rules/gates.md); the checks live in
`scripts/krkr_gates/checks/`.

**The Android target is never built here.** The `platform-android` feature
builds the platform-independent helpers plus a typed stub *on the host*, and
everything that needs `ndk` stays gated to `target_os = "android"`. There is
no NDK on the development host and none may be installed (see §14 of
`TODO.md`); the Android target is compiled and run on a machine that has one,
and Android-only code that has not been compiled is documented as unverified
rather than described as working. Do not download or install the Android SDK,
the NDK or any Android toolchain here — the Android build belongs to the
`apk` CI job and to a machine that has the toolchain, so Kotlin launcher
changes are reviewed by reading. The gate covers the Rust side only;
`krkr-gate android-target` resolves the dependency graph without compiling,
and fails when a desktop-only backend feature leaks into the Android target,
or when two mutually-exclusive backends are enabled together.

### 0.5 Release tags are metered

`.github/workflows/release.yml` triggers on `v*` **tags** and on
`workflow_dispatch` (a branch push triggers nothing). One `v*` tag builds
the Linux x86_64 binary and the Android APK, then a single final job
publishes both to the same GitHub Release. The workflow is build-only: it
does **not** rerun the static gate or the tests, so the tag must point at a
commit that already passed them locally. Each tag still costs CI minutes, so
commit locally, keep the gate green, and **hold the tag until the user says
the build is wanted**. Do not tag and push on every green gate — batch the
work, report what is ready, and wait. `krkr-gate ci` fails if the workflow
stops matching this contract.

---

## 1. Start here

### 1.1 What this project is

krkr-rs is a native Rust reimplementation of the KiriKiri2 (TVP) visual-novel
engine, running on Linux and Android. It is a **compatible emulator**, not a
port: the goal is that unmodified games and scripts behave exactly as they do
on the reference engine, and the reference is the oracle for behaviour. The
reference's own C++ TJS2 VM is vendored behind a thin Rust FFI, so unmodified
scripts and save files work; the archives, the image and audio decode, the
renderer and the scenario engine are reimplemented in Rust.

### 1.2 Get it running

The toolchain is Rust stable (edition 2024), a C++17 compiler, Zig 0.17
(`crates/tjs/build.rs` fetches the C++ dependencies with it), bison, Python 3
and the system development packages for FFmpeg, ALSA and libc++. The Linux CI
job installs exactly that list: `bison build-essential clang cmake
libclang-dev`, `libavcodec-dev libavformat-dev libavutil-dev
libswresample-dev libswscale-dev`, `libasound2-dev libc++-dev libc++abi-dev
pkg-config python3`.

Build and run:

```bash
cargo build                                           # target/debug/krkr
./target/debug/krkr --base-dir "/path/to/game"        # a game directory
./target/debug/krkr --headless --frames 1000 --base-dir "/path/to/game"
```

`--headless` needs neither a display nor a GPU: it runs the frame loop
offscreen and prints the null backend's frame statistics. The full option
table is in [`docs/code/app/main.md`](docs/code/app/main.md); `krkr --help`
prints it. A game directory is mounted through its `*.xp3` archives: with no
explicit `--archive`, every `*.xp3` under it is mounted recursively; with an
explicit list, that list is authoritative and later archives win (§7, D35).
The test suite and the benchmarks use three environment variables —
`KRKR_REPO` (the checkout root), `KRKR_TEST_GAME` (a game directory) and
`KRKR_TEST_SAVE` (a real KAG save bitmap) — described in
[`scripts/README.md`](scripts/README.md).

A plain build does **not** need the reference checkout (§1.3): the C++ VM is
vendored under `crates/tjs/cpp/tjs2/`. `scripts/vendor-tjs2.sh` does need it,
and it re-applies `scripts/patches/tjs2-krkr-rs.patch` — every change under
`crates/tjs/cpp/tjs2/` must be recorded in that patch, or the next re-vendor
drops it.

### 1.3 Get the reference

The reference engine is **not** part of this repository. It is a separate
checkout, cloned beside it:

```bash
git clone https://github.com/2468785842/krkr2 reference
```

`/reference/` is git-ignored. Throughout this document and the test suite,
*the reference* means that checkout: a reference citation is the path of a
file inside it, relative to the clone root, followed by the line — for
example `reference/cpp/core/visual/LayerIntf.cpp:1234`.

### 1.4 Verify a change

`uv run --project scripts krkr-gate gate` (the ordered static gate) and
`uv run --project scripts krkr-gate test` (the test suites) must both be green
before a change is done. §0.4 has the order, the check list and what each
check protects; [`docs/rules/gates.md`](docs/rules/gates.md) has the full
contract.

### 1.5 Where the code lives, and what you must not break

One page per module, grouped by crate: [`docs/code/README.md`](docs/code/README.md).
The architecture, the crate map and the frame are in
[`docs/arch/overall.md`](docs/arch/overall.md); §2 below has the crate table.

The rules you must not break are these sections: §2 (the layers and the one
dependency direction), §3 (the decisions D9 … D41), §4 (no silent fallbacks),
§5–§6 (the house style and the comment rules), §7 (what belongs in L2 and
what stays in L3), §8 (every behaviour test cites the reference), D29 (the
surface lock) and §0.5 (release tags are metered). §11 says where each is
written out in full.

---

## 2. The four layers and the one allowed dependency direction

```
L4  vm       TJS VM host (Tjs2Engine, GC, bytecode, script cache, frame body)
L3  compat   KiriKiri compatibility surface: native classes, KAG, event adapter,
             static plugins, Win32 emulation
L2  engine   a small, generic game engine: the layer system/scene graph, bitmaps,
             sound mixer, timers, VFS namespace; owns engine state
L1  platform api · fs · media · psd · render · platform
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
- **L3 names the engine only through `crates/compat/src/engine_api.rs`.** That
  file is the L3↔L2 protocol surface; the `l3-engine-api` rule inside the
  `layering` check rejects `engine::` anywhere else under `compat/src`. A new
  engine item L3 needs is re-exported there and recorded in
  [`docs/rules/api_rules.md`](docs/rules/api_rules.md) §4.
- **`#[cfg(target_os)]` and platform crates live only in L1 (`platform`,
  `media`).**

Crate roles:

| crate | path | layer | role |
|---|---|---|---|
| `tjs` | `crates/tjs` | vendored | C++ TJS2 VM FFI (unchanged, all-FFI) |
| `api` | `crates/platform/api` | L1 | shared DTOs + typed `*Error` enums, no impls |
| `fs` | `crates/platform/fs` | L1 | stateless `FileSystem`/`FileState` + XP3 read/parse |
| `media` | `crates/platform/media` | L1 | stateless `Media` + image/audio/video decode |
| `psd` | `crates/platform/media/psd` | L1 | stateless PSB container + PSD image decode (`forkunion`/`fearless_simd`) |
| `render` | `crates/platform/render` | L1 | stateless `RenderPipeline`/`FontSystem` + Vulkan |
| `platform` | `crates/platform/platform` | L1 | window/surface, event loop, input, clock, threads |
| `engine` | `crates/engine` | L2 | scene graph, mixer, timers, VFS, projection |
| `compat` | `crates/compat` | L3 | native classes, KAG, event adapter, plugins |
| `vm` | `crates/vm` | L4 | VM host |
| `app` | `crates/app` | root | the desktop binary: wiring only, no platform logic |
| `android` | `crates/android` | root | the Android composition root (JNI, `GameActivity`) |
| `testkit` | `crates/testkit` | dev | shared test helpers, dev-dependencies only |

`psd` is a workspace member that lives inside the `media` directory, which
excludes it; `app` and `android` are the two composition roots, and both are
wiring only.

### 2.1 The sizes: L3 is the big layer, and that is the design

Rust source lines per crate (`*.rs` under the crate, tests excluded; `tjs`
counts its `src/lib.rs` and its `build.rs`):

| layer | crates | total |
|---|---|---|
| L1 | api 2 924 · fs 2 906 · media 5 405 · psd 4 113 · render 10 836 · platform 3 831 | 30 015 |
| L2 | engine | 30 357 |
| L3 | compat | 98 740 |
| L4 | tjs 7 128 · vm 868 | 7 996 |

L3 is more than three times L2. **That is correct and intended.** L2 is a
small, generic, backend-agnostic engine — the layer tree, the mixer, the
timers, the VFS namespace, the projection — and keeping it small is exactly
what lets the backend be swapped freely: the Vulkan host, the null backend
and the Android host all drive the same L2, and none may change it. L3 is the
whole TVP abstraction and compatibility engine — the native classes, the KAG
scenario, the event adapter, the plugins, the Win32 emulation — and it is
large *because* it carries the reference's surface: every script-visible
member has to exist, and has to behave like the reference.

So "L3 is bigger than L2" is a property to be proud of, not a smell to be
fixed. The rule that follows is D16: every L3 **member** is a thin marshalling
shim over L2 — thin per member, not per layer. L3 never grows a second engine,
and L2 never takes on TVP vocabulary to make L3 smaller. Measure a crate with
`find crates/compat/src -name '*.rs' | xargs wc -l | tail -1`.

---

## 3. The non-negotiable decisions (D-rules)

The full table, with each rule as it binds the tree today, is
[`docs/rules/arch_rules.md`](docs/rules/arch_rules.md) §13; the summary
below is the short form, for the rules that come up in review. The **code
must not cite them by number** (§6) — they are for the people reading the
tree. The live target for the current work is the presentation contract in
[`docs/arch/l2.md`](docs/arch/l2.md) §4 and the frame contract in the same
file §9.

- **D9 — L1 is stateless.** L1 objects are facades over platform capabilities:
  `&self` methods, and every evolving value passed as an explicit `&mut`
  state value (`RenderState`, `FileState`, `DecoderState`, …). No
  `static mut`, `thread_local!`, `LazyLock`/`OnceLock`/`OnceCell`, no
  process-global registries, no hidden caches, no `&mut self` on an L1
  service. All engine state is owned by L2. Guarded by `krkr-gate stateless`.
- **D10 — L1 owns traversal/rendering.** L2 keeps the `RenderScene` tree
  current and calls `render`; it never builds a draw list. L1 does the
  traversal, composition, batching, blend selection and drawing.
- **D11 — no silent fallbacks, no hardcoded limits.** Where the intended
  behaviour cannot be performed — unsupported feature, malformed or
  overflowing data, unrepresentable size — return a **typed error**. No
  degraded substitutes, no arbitrary size caps, no "guess and continue", no
  `unwrap_or`/`_ =>` that hides a failure. The **one** documented exception
  is the null-plugin object (D14), which logs a warning on every call.
- **D13 — the platform and the media backend are chosen at compile time.**
  Exactly one of `platform-linux` / `platform-android`; the media backend
  follows it: `video-ffmpeg` on Linux, `video-mediacodec` on Android.
- **D14 — native plugins are static, built into L3.** One `BUILTIN_PLUGINS`
  table; `Plugins.link(name)` recognizes every ported DLL. A DLL we have
  **not** ported gets the null-plugin object: it accepts every member/call,
  returns void, logs `warn!` per call and never throws. This is not silent.
- **D15 — loading is explicit.** The archive list comes from the command line
  (repeatable, later archives win); the startup script is found inside the
  mounted archives, and a missing startup is a typed error. D35 adds the one
  opt-in extension.
- **D16 — L2 stays generic; every L3 member stays thin.** Every L3 native
  member reduces to *marshal args → one L2 call → marshal result*. Real logic
  (loops, state machines, blending, timing, scheduling) belongs in L2 unless
  it is genuinely TVP-specific. Do **not** push TVP specifics into L2 to
  shrink L3; do **not** grow L3 into a second engine. "Thin" is a property of
  a member, not of the layer: the layer is large because the reference's
  surface is (§2.1).
- **D18 — emulate the Win32 environment.** Games are Windows-native and probe
  Win32 APIs. `HWND`, draw devices, message receivers, registry, shell and
  system metrics must return **emulated** values and handles — synthetic,
  stable integers — never typed errors. A typed error there is a
  compatibility bug.
- **D26 — L2 owns the frame.** `Engine::step(dt_ms, hooks)` /
  `Engine::frame(now_ms, hooks)` own the frame clock and the frame entry
  point: they advance the L2 timer registry, run the single
  `GameFrame::before_render(now_ms, dt_ms)` hook, then project and render.
  Nothing above L2 owns a `dt`; a deterministic test drives `frame(now)`.
  Landed: the engine, the timer pump, projection/render ownership and the
  hook. Staged: the mixer, video and transition pumps still run inside the
  L3 hook — keep deterministic test clocks for them.
- **D27 — the L2 timer is `create_interval`/`set_interval`/`set_enabled`/`remove`.**
  No config struct, no mode enum, no fire list to hand upward.
- **D28 — no game-specific code.** A behaviour change must implement what the
  reference does. Never special-case one title to make it work.
- **D29 — the surface lock is the contract.** `crates/compat/tests/runtime_surface.rs`
  dumps what `CompatHost::install` actually registered; `krkr-gate surface-lock`
  fails on any added/removed member. L3 internals are free to change only
  while the dump is unchanged; adding a member needs a reviewed baseline
  update.
- **D30 — tests cite the reference; the code does not.** Every behaviour
  assertion carries a reference citation (§1.3). Implementations owe no
  citation and no resemblance to the C++.
- **D31 — comments describe the code as it is.** No history, no "used to", no
  design rationale, no decision numbers (`D9`, `D31`, …), no cross-links to
  docs. Keep `// SAFETY:`, non-obvious invariants, and reference citations
  **in tests**. Tests are exempt from the jargon rule.
- **D32 — only the Android backend may be stubbed.** L3 has no stubs and no
  script-observable approximations: every member either behaves like the
  reference or raises because the reference raises. Android is the one backend
  no script can observe. `#[allow(dead_code)]` is banned (one exception:
  `crates/compat/tests/common/mod.rs`).
- **D33 — L1 may change the `engine` signatures it names.** `engine` depends
  on the L1 type crates (`api`, `fs`, `render`) and its public API may use
  their types. An L1 type change that flows into an `engine` signature is
  accepted; the L2 surface is frozen by review and audit, not by a snapshot.
- **D34 — rendering may take the efficient path when it looks near-identical;
  script-observable semantics stay exactly reference.** Pixel rasterization
  (hatch/gradient fills, GPU composition, transitions) may approximate
  efficiently if the visible output is close. Every value a script can observe
  — member getters/setters, return values, raised errors, `typeof`, string
  and number conversions — must match the reference exactly. This is the
  boundary of D32's ban.
- **D35 — a directory input mounts every `*.xp3` under it.** Only when no
  explicit `--archive` is given: recursive discovery, ordered by file name
  (case-insensitive) then shallower-first, later mounts win. Symbolic-link
  directories are not followed; the explicit list stays authoritative.
- **D36 — a compatibility claim names its evidence and scope.** A test is
  evidence for what it asserts and nothing else; a knowingly-left divergence
  is recorded as an **approximation** with its reference gap, never as
  "unobservable". An engine release needs a real title run on a real device
  with the log captured (`docs/code/android/README.md`).
- **D37 — a pointer is never a handle.** No sign test, no narrowing, no
  hidden pointer property; an id crosses script-visible state.
- **D38 — where the reference is a binary, the DLL is the oracle and is cited
  by hash.** An unported plugin's behaviour test cites the shipped DLL build —
  the DLL's file name, the `sha256[:12]` of the build and the function or
  address it came from — and gates on a committed derived fixture, never on
  the DLL at run time. The game DLLs are not committed to this repository.
- **D39 — presentation is one rule: Fit with letterbox.** Scale uniformly by
  `min(device/vm)`, centre, clear the margin. There is no mode enum and no
  `scaling` configuration key on any launcher; the old scaling feature is
  deleted rather than adapted (`docs/arch/l2.md` §4).
- **D40 — a pixel-translation layer in L2 separates VM pixels from device
  pixels.** L3 and scripts speak VM pixels; L2 owns the Fit mapping and
  computes one root transform for the projected render tree. L1 receives that
  tree and composes the root transform during traversal; it never interprets
  VM semantics. The framebuffer is the single device-resolution backing; a VM
  pixel read samples its device region. At `s == 1` the mapping is the
  identity; script-observable values stay VM and reference-exact (D34).
  Contract: `docs/arch/l2.md` §4, `docs/code/engine/translate.md`.
- **D41 — the VM↔device translation is embedded in L2; L3 never sees it.**
  Every L2 readback method takes VM coordinates and resolves them through the
  current `PixelTranslation`; the projected render tree keeps VM coordinates
  and carries the mapping on its root transform. L3 natives keep their
  VM-coordinate contract: they must not call
  `to_device_*`/`to_vm_point`/`raster_scale`, name a device pixel, or
  import `PixelTranslation`. The composition root passes platform input
  through an L2 mapping method; L3 receives only VM events.

---

## 4. Errors, fallbacks and "slop" — what to block

The single most important review criterion is: **does this change introduce a
silent fallback?** Block and fix any of these:

- A wildcard `_ =>` arm that swallows a newly-added enum variant instead of
  handling it (D11: prefer an exhaustive match so a new variant is a compile
  error). Enums L3 matches variant-by-variant must **not** be
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
- `#[allow(clippy::…)]` / `#![allow(clippy::…)]` / `#[expect(clippy::…)]`,
  tests included. Fix the lint or write the code differently; the
  `house-style` gate has no allowlist exemption for one.
- A stub `todo!`/`unimplemented!`/approximation in L3 (D32). Android-only stubs
  are the sole exception.

Typed errors: one `#[non_exhaustive]` error enum per domain
(`FsError`, `MediaError`, `RenderError`, `GlyphError`, `AudioError`,
`TextError`, `SoundError`, `TimerError`, `VideoError`, `TransError`, …), owned
by the crate that declares it. Errors are values; no panics on bad input.

---

## 5. Idiomatic Rust (this codebase's house style)

The detailed review/rewrite checklist is
[`docs/rules/code_styles.md`](docs/rules/code_styles.md).
It elaborates this section; if they disagree, this file wins.

- **No OOP bean accessors.** A setter whose body is only `self.field = arg`
  must be a public field. A getter for a public field is deleted. Accessors
  exist only to enforce an invariant or do real work, and are named for the
  work (`clamp_opacity`, `set_interval`), never `get_foo`/`set_foo` when they
  only move a field. Getters that read a real property are `foo()`, not
  `get_foo()`.
- Prefer explicit values and `&self` actions; avoid hidden `&mut self` state on
  service objects.
- Parameters: `&str`, `&[T]`, `&mut [T]`, `impl Into<…>`; never `&String`,
  `&Vec<T>`, or `&Box<T>`. A sequence that is only iterated is
  `impl IntoIterator<Item = …>`, and a sequence result that is only iterated
  is `impl Iterator<Item = …>`; `&[T]`/`Vec<T>` stay where the slice or buffer
  itself is the contract (indexing, exact length, FFI).
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
- A **production** comment must not cite the external engine or narrate the
  port: no path into the reference clone, no `*.cpp:line` or `.h`/`.hpp`
  filename, and none of the words `reference`, `oracle`, `C++`, `ported`,
  `port of`, `upstream`, `TVP`, `KiriKiri`. The `house-style` gate enforces
  this for non-test comments. Test comments keep the citation. Comments that
  predate the rule are baselined in
  `scripts/allowlists/house_style_baseline.txt` (burn-down tracked in
  `TODO.md` §15); a new one fails.
- No internal jargon in **printed/logged** text (`L1`–`L4`, decision numbers).
  Use `platform`, `engine`, `compat`, `app`, `tjs`, `vm`, or plain English.
  Tests are exempt.
- Reference citations belong in tests, in the form of §1.3: the reference
  path and the line.
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
- If a single L3 file grows larger than the L2 module it wraps, the abstraction
  is leaking; push generic behaviour down (or the shared behaviour struct into
  L2). This is a rule about one file against the module under it — not about
  the size of the layer, which is meant to be large (§2.1).
- Shared behaviour is a **struct composed** by several thin L3 classes, never
  a registration list copied per class.

---

## 8. Tests

- Tests live in `tests/` (integration) or `#[cfg(test)]` modules, and every
  behaviour assertion cites the reference file and line (§1.3).
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
9. The vendored C++ can be a reverse-engineered reconstruction and can
   contradict the shipped data; when it does, say so explicitly with the
   evidence.
10. TJS2 does not bind `function` declarations inside an anonymous function
    body (no IIFE wrappers); use a named function, call it, delete it.
11. `Layer.id`/`nativeId`, `Bitmap.id`, `Window.id`, `Font.id` do not exist by
    design; identify through real members or the L2 `Scene`.
12. Loading is explicit (`--archive`, repeatable; later wins). There is **no** folder scan in the reference (D15), but D35 adds one opt-in exception: a directory-only launch (`--base-dir`/positional dir, no `--archive`) mounts every `*.xp3` found recursively (`engine::storage::discover_archives`).
13. The frame is L2's: `GameHost::step` must not accumulate a `now_ms` or call the subsystem pumps itself. Pass events through `Engine::step(dt, hooks)`; the hook is `VmHook` (VM tick + audio drain). `compat::run_frame_body` starts by draining the timer posts the engine already produced; it must not call `TimerState::advance`.
14. VM pixels are not device pixels, and the logical screen is not the window client. Layer rects, bitmap dimensions, `Font.size`, `getPixel` and `System.screenWidth` are the logical screen (the primary layer); `Window.innerWidth`/`innerHeight` are the window client; the framebuffer is device space. Only `engine::translate::PixelTranslation` names both; the render tree keeps VM coordinates and carries the Fit mapping on its root transform, which L1 composes without interpreting it. An OS resize changes the framebuffer only — never write it back into the scene window; a script window-size change goes the other way through `Engine::take_window_resize`, and the translation fits the unchanged screen into the new client.
15. The input adapter's dispatch budget is **wall-clock time** (4 ms by default), so a frame that dispatches several events can cut the sequence short on a loaded machine and defer the rest to a frame the caller never drives. The default is a presentation policy for the real game; a test that drives an explicit event sequence uses the harness adapter, whose budget does not defer. A test that exercises the budget itself builds its own `EventAdapter::with_dispatch_budget`.

---

## 10. Definition of done for a change

1. Behaviour matches the reference (or raises exactly where it does), and
   every behaviour test cites it.
2. No silent fallback, no arbitrary cap, no `#[allow(dead_code)]`, no L3 stub
   or approximation.
3. Layer and dependency rules hold (no platform code in L3, no TVP vocabulary
   in L2, no L2/L3 knowledge in L1).
4. The surface lock and both parity reports are unchanged (or the baseline
   change is deliberate and reviewed).
5. Comments/D31 clean; public items documented; `cargo fmt` clean.
6. Superseded code, tests, allowlist entries and docs are deleted in the same
   change; nothing is left as a compatibility shim, a dead branch or a stale
   section.
7. The integrator's full gate is green.

---

## 11. Where to read more (read on demand, do not re-derive)

The rules are stated in [`docs/rules/arch_rules.md`](docs/rules/arch_rules.md)
and the design that produced them is in [`docs/arch/`](docs/arch/). The long
decision log and the read-only audits that preceded this tree are **not part
of this repository**, so a rule here cites the page that states it, not the
page that explains why it was taken — the *why* is in the tracked pages, or
in the commit that introduced the change.

| Topic | Document |
|---|---|
| The four layers, the crate map, the coordinate spaces, the frame | `docs/arch/overall.md` |
| L1 — the stateless platform core | `docs/arch/l1.md` |
| L2 — the engine: the layer tree, the projection, the frame | `docs/arch/l2.md` |
| The L2 public API: the engine crate's types, operations, error types | `docs/arch/l2-api.md` |
| L3 — the compatibility surface, the plugins, the events | `docs/arch/l3.md` |
| L4 — the TJS VM host | `docs/arch/l4.md` |
| The Vulkan pipeline and the blend mapping | `docs/arch/l1.md` §3, `docs/code/platform/render.md` |
| The media engine | `docs/arch/l1.md` §4–§5, `docs/code/platform/media.md` |
| The file system | `docs/code/platform/fs.md` |
| Layers, state, frame, coordinate spaces, errors, the decisions | `docs/rules/arch_rules.md` |
| Public surfaces, the L3↔L2 protocol, ownership, error enums | `docs/rules/api_rules.md` |
| The L3 ↔ L2 protocol, itemised per module | `docs/code/compat/engine_api.md` |
| The Rust reviewer checklist | `docs/rules/code_styles.md` |
| The ordered gate, the test suites, the release pipeline | `docs/rules/gates.md` |
| The module map (one page per module) | `docs/code/README.md` |
| The gate checks and the environment variables | `scripts/README.md` |
| The L3 plugins and the null plugin | `docs/code/compat/plugin.md` |
| Game loading, precedence, startup order | `docs/code/engine/storage.md`, `docs/code/app/game.md` |
| The Android backends (AAudio, MediaCodec) | `docs/code/android/README.md` |
| Presentation (Fit only) and the VM↔device translation | `docs/arch/l2.md` §4, `docs/code/engine/translate.md` |
| Surface lock / parity | `docs/native_parity.md`, `docs/plugin_parity.md` |
| The open work, and the known gaps | `TODO.md`, `docs/arch/l3.md` §9 |
